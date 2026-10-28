//! Where the app keeps its app data (recovery records, the update-attempt note) and its WebView2 profile.
//!
//! Release builds always use Tauri's folders under the user profile. Debug builds (local runs, `cargo test`, the
//! UI Automation verifiers) may set `SMACROBAT_TEST_DATA_DIR` to an absolute folder; the app then keeps its app data in
//! `<folder>\app-data` and the WebView2 profile in `<folder>\webview`, so nothing is written to the user profile. The
//! override, its variable name and the code that reads it are compiled out of release builds.

use std::path::PathBuf;
use tauri::Manager;

/// Debug builds only: the absolute folder that replaces the app-data and WebView2 folders.
#[cfg(any(test, debug_assertions))]
pub(crate) const TEST_DATA_DIR_VARIABLE: &str = "SMACROBAT_TEST_DATA_DIR";

/// Whether this build honours the override at all.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const OVERRIDE_AVAILABLE: bool = cfg!(debug_assertions);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TestDataDirs {
    pub(crate) app_data: PathBuf,
    pub(crate) webview: PathBuf,
}

/// The override for this process: always `None` in release builds. In debug builds `None` when the variable is unset
/// or empty, and an error when it is set to anything but an absolute path, so a typo never falls back to the profile.
pub(crate) fn test_override() -> Result<Option<TestDataDirs>, String> {
    #[cfg(debug_assertions)]
    {
        override_from(std::env::var_os(TEST_DATA_DIR_VARIABLE))
    }
    #[cfg(not(debug_assertions))]
    {
        Ok(None)
    }
}

#[cfg(any(test, debug_assertions))]
fn override_from(value: Option<std::ffi::OsString>) -> Result<Option<TestDataDirs>, String> {
    let Some(value) = value.filter(|value| !value.is_empty()) else { return Ok(None) };
    let plain = !value.to_string_lossy().split(['\\', '/']).any(|part| part == "." || part == "..");
    let root = PathBuf::from(value);
    if !root.is_absolute() || !plain {
        return Err(format!("{TEST_DATA_DIR_VARIABLE} must be an absolute folder without . or .. parts."));
    }
    Ok(Some(TestDataDirs { app_data: root.join("app-data"), webview: root.join("webview") }))
}

/// The app-data folder: the debug override when set, otherwise Tauri's `app_data_dir`.
pub(crate) fn app_data_dir<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, String> {
    resolve_app_data(test_override()?, || app.path().app_data_dir().map_err(|error| error.to_string()))
}

fn resolve_app_data(test_dirs: Option<TestDataDirs>, default: impl FnOnce() -> Result<PathBuf, String>) -> Result<PathBuf, String> {
    match test_dirs {
        Some(dirs) => Ok(dirs.app_data),
        None => default(),
    }
}

/// Debug builds with the override: stop Tauri creating the configured windows (it would give them the profile's
/// WebView2 folder) so `create_windows` can create them with the override's folder.
#[cfg(debug_assertions)]
pub(crate) fn defer_configured_windows(context: &mut tauri::Context<tauri::Wry>) {
    for window in context.config_mut().app.windows.iter_mut() {
        window.create = false;
    }
}

/// Debug builds with the override: create the configured windows with their WebView2 profile under the override.
#[cfg(debug_assertions)]
pub(crate) fn create_windows(app: &tauri::App, dirs: &TestDataDirs) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&dirs.webview)?;
    for config in app.config().app.windows.clone() {
        tauri::WebviewWindowBuilder::from_config(app.handle(), &config)?.data_directory(dirs.webview.clone()).build()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn override_exists_only_in_debug_builds() {
        assert_eq!(OVERRIDE_AVAILABLE, cfg!(debug_assertions));
        #[cfg(not(debug_assertions))]
        assert_eq!(test_override(), Ok(None));
    }

    #[test]
    fn unset_or_empty_override_keeps_the_default_folders() {
        assert_eq!(override_from(None), Ok(None));
        assert_eq!(override_from(Some(OsString::new())), Ok(None));
        let default = resolve_app_data(None, || Ok(PathBuf::from(r"C:\Users\someone\AppData\Roaming\local.pdfworkstation.desktop")));
        assert_eq!(default.unwrap(), Path::new(r"C:\Users\someone\AppData\Roaming\local.pdfworkstation.desktop"));
    }

    #[test]
    fn absolute_override_moves_app_data_and_webview_under_it() {
        let dirs = override_from(Some(OsString::from(r"E:\Coding Projects\Smacrobat\tmp\run"))).unwrap().unwrap();
        assert_eq!(dirs.app_data, Path::new(r"E:\Coding Projects\Smacrobat\tmp\run\app-data"));
        assert_eq!(dirs.webview, Path::new(r"E:\Coding Projects\Smacrobat\tmp\run\webview"));
        let resolved = resolve_app_data(Some(dirs.clone()), || panic!("the default folder must not be consulted")).unwrap();
        assert_eq!(resolved, dirs.app_data);
    }

    #[test]
    fn relative_or_dotted_override_is_refused_not_ignored() {
        for value in ["relative", r"\rooted-no-drive", r"E:\tmp\..\escape", r"E:\tmp\.\here", "C:drive-relative"] {
            let error = override_from(Some(OsString::from(value))).unwrap_err();
            assert!(error.contains(TEST_DATA_DIR_VARIABLE), "{value}: {error}");
        }
    }
}
