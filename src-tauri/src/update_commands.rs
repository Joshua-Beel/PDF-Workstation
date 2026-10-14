use serde::{Deserialize, Serialize};
use std::{sync::Mutex, time::Duration};
use tauri::{ipc::Channel, AppHandle, Manager, Runtime, State};
use tauri_plugin_updater::{Update, UpdaterExt};

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// The only update facts the webview may send back: which offer it is acting on.
/// Endpoint, public key, proxy, headers, target and version policy come from the
/// backend and `tauri.conf.json`; unknown fields are rejected.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UpdateKey {
    pub version: String,
    pub signature: String,
    /// The backend's number for the check that produced this offer, so two
    /// checks of the same release are still different offers.
    pub check: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOffer {
    pub current_version: String,
    pub version: String,
    pub body: Option<String>,
    pub signature: String,
    pub check: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", content = "data")]
pub enum DownloadEvent {
    #[serde(rename_all = "camelCase")]
    Started { content_length: Option<u64> },
    #[serde(rename_all = "camelCase")]
    Progress { chunk_length: usize },
    Finished,
}

enum Stage<U, B> {
    Empty,
    Checked(UpdateKey, U),
    Downloading(UpdateKey),
    Downloaded(UpdateKey, U, B),
}

/// Holds the single update the backend itself fetched in the latest check.
/// Download and install act only on that update, matched by version and
/// signature; install consumes it.
pub struct UpdateSlot<U, B> {
    stage: Stage<U, B>,
    latest_check: u64,
    /// True while an install confirmation is open, so only one can be shown.
    confirming: bool,
}

impl<U, B> Default for UpdateSlot<U, B> {
    fn default() -> Self {
        Self { stage: Stage::Empty, latest_check: 0, confirming: false }
    }
}

impl<U, B> UpdateSlot<U, B> {
    /// Clears any previous offer and numbers the new check. Only the latest
    /// check may store its result.
    pub fn begin_check(&mut self) -> Result<u64, String> {
        if matches!(self.stage, Stage::Downloading(_)) {
            return Err("An update download is already in progress.".into());
        }
        self.stage = Stage::Empty;
        self.latest_check += 1;
        Ok(self.latest_check)
    }

    pub fn offer(&mut self, key: UpdateKey, update: U) -> Result<(), String> {
        if matches!(self.stage, Stage::Downloading(_)) {
            return Err("An update download is already in progress.".into());
        }
        if key.check != self.latest_check {
            return Err("A newer update check replaced this one.".into());
        }
        self.stage = Stage::Checked(key, update);
        Ok(())
    }

    /// Clears the slot only if it holds the offer named by `key`; another
    /// check's offer is left alone. Returns whether anything was released.
    pub fn release(&mut self, key: &UpdateKey) -> bool {
        let matches = match &self.stage {
            Stage::Checked(held, _) | Stage::Downloading(held) | Stage::Downloaded(held, ..) => held == key,
            Stage::Empty => false,
        };
        if matches {
            self.stage = Stage::Empty;
        }
        matches
    }

    pub fn begin_download(&mut self, key: &UpdateKey) -> Result<U, String> {
        match std::mem::replace(&mut self.stage, Stage::Empty) {
            Stage::Checked(held, update) if held == *key => {
                self.stage = Stage::Downloading(held);
                Ok(update)
            }
            Stage::Checked(held, update) => {
                self.stage = Stage::Checked(held, update);
                Err("This update no longer matches the latest check. Check for updates again.".into())
            }
            Stage::Downloaded(held, update, bytes) if held == *key => {
                self.stage = Stage::Downloading(held);
                drop(bytes);
                Ok(update)
            }
            other => {
                let message = if matches!(other, Stage::Downloading(_)) { "An update download is already in progress." } else { "No checked update is available. Check for updates again." };
                self.stage = other;
                Err(message.into())
            }
        }
    }

    /// Returns false when the slot was released or replaced during the download.
    pub fn finish_download(&mut self, key: &UpdateKey, update: U, bytes: Result<B, ()>) -> bool {
        match &self.stage {
            Stage::Downloading(held) if held == key => {
                let held = held.clone();
                self.stage = match bytes {
                    Ok(bytes) => Stage::Downloaded(held, update, bytes),
                    Err(()) => Stage::Checked(held, update),
                };
                true
            }
            _ => false,
        }
    }

    fn not_installable(&self) -> String {
        match &self.stage {
            Stage::Downloading(_) => "The update download has not finished.",
            Stage::Downloaded(..) | Stage::Checked(..) => "This update was not downloaded by the latest check. Check for updates again.",
            Stage::Empty => "No downloaded update is available. Check for updates again.",
        }
        .into()
    }

    /// The version of the downloaded update held for `key`, without taking it.
    pub fn downloaded_version(&self, key: &UpdateKey) -> Result<String, String> {
        match &self.stage {
            Stage::Downloaded(held, ..) if held == key => Ok(held.version.clone()),
            _ => Err(self.not_installable()),
        }
    }

    pub fn take_for_install(&mut self, key: &UpdateKey) -> Result<(U, B), String> {
        match std::mem::replace(&mut self.stage, Stage::Empty) {
            Stage::Downloaded(held, update, bytes) if held == *key => Ok((update, bytes)),
            other => {
                self.stage = other;
                Err(self.not_installable())
            }
        }
    }
}

pub fn is_upgrade(current: &semver::Version, offered: &str) -> bool {
    semver::Version::parse(offered).map(|offered| offered > *current).unwrap_or(false)
}

pub type PendingUpdate = Mutex<UpdateSlot<Update, Vec<u8>>>;

fn lock<'a>(state: &'a State<'_, PendingUpdate>) -> Result<std::sync::MutexGuard<'a, UpdateSlot<Update, Vec<u8>>>, String> {
    state.lock().map_err(|_| "The updater state is unavailable. Restart PDF Workstation and try again.".to_string())
}

#[tauri::command]
pub async fn check_for_update<R: Runtime>(app: AppHandle<R>, state: State<'_, PendingUpdate>) -> Result<Option<UpdateOffer>, String> {
    let check = lock(&state)?.begin_check()?;
    let updater = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .version_comparator(|current, release| release.version > current)
        .build()
        .map_err(|error| error.to_string())?;
    let Some(update) = updater.check().await.map_err(|error| error.to_string())? else { return Ok(None) };
    if !is_upgrade(&app.package_info().version, &update.version) {
        return Ok(None);
    }
    let offer = UpdateOffer { current_version: update.current_version.clone(), version: update.version.clone(), body: update.body.clone(), signature: update.signature.clone(), check };
    lock(&state)?.offer(UpdateKey { version: offer.version.clone(), signature: offer.signature.clone(), check }, update)?;
    Ok(Some(offer))
}

#[tauri::command]
pub async fn download_update(state: State<'_, PendingUpdate>, update: UpdateKey, on_event: Channel<DownloadEvent>) -> Result<(), String> {
    let mut pending = lock(&state)?.begin_download(&update)?;
    pending.timeout = Some(DOWNLOAD_TIMEOUT);
    let mut first_chunk = true;
    let result = pending
        .download(
            |chunk_length, content_length| {
                if first_chunk {
                    first_chunk = false;
                    let _ = on_event.send(DownloadEvent::Started { content_length });
                }
                let _ = on_event.send(DownloadEvent::Progress { chunk_length });
            },
            || {
                let _ = on_event.send(DownloadEvent::Finished);
            },
        )
        .await;
    let (bytes, outcome) = match result {
        Ok(bytes) => (Ok(bytes), Ok(())),
        Err(error) => (Err(()), Err(error.to_string())),
    };
    if !lock(&state)?.finish_download(&update, pending, bytes) {
        return Err("The update was released before its download finished. Check for updates again.".into());
    }
    outcome
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstallOutcome {
    Cancelled,
    Started,
}

/// An `install_update` failure. `offer_kept` tells the webview the backend
/// still holds the downloaded offer (nothing was taken or installed), so it
/// must not release it and download again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallError {
    pub message: String,
    pub offer_kept: bool,
}

impl InstallError {
    fn kept(message: impl Into<String>) -> Self {
        Self { message: message.into(), offer_kept: true }
    }
}

impl From<String> for InstallError {
    fn from(message: String) -> Self {
        Self { message, offer_kept: false }
    }
}

fn lock_slot<U, B>(slot: &Mutex<UpdateSlot<U, B>>) -> Result<std::sync::MutexGuard<'_, UpdateSlot<U, B>>, String> {
    slot.lock().map_err(|_| "The updater state is unavailable. Restart PDF Workstation and try again.".to_string())
}

/// Clears the open-confirmation flag however the install attempt ends.
struct ConfirmingGuard<'a, U, B>(&'a Mutex<UpdateSlot<U, B>>);

impl<U, B> Drop for ConfirmingGuard<'_, U, B> {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).confirming = false;
    }
}

/// The confirmation must be owned by the main window so it is modal to the app.
/// Without that window the install is refused rather than shown ownerless.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn require_owner(owner: Option<isize>) -> Result<isize, InstallError> {
    owner.filter(|hwnd| *hwnd != 0).ok_or_else(|| InstallError::kept("The update confirmation could not be attached to the PDF Workstation window. Nothing was installed. Close and reopen PDF Workstation, then try again."))
}

/// Asks before installing the downloaded update the backend holds for `key`.
/// The question names the backend's own version, not one supplied by the
/// webview. The slot is not locked while the question is open; No leaves the
/// downloaded offer in place, Yes takes it single use and installs it. Only one
/// question may be open at a time: a second call while one is pending is refused.
/// `confirm` returns `Err` when the question could not be shown; that is a
/// refusal that keeps the offer, never a No.
pub fn install_confirmed<U, B>(slot: &Mutex<UpdateSlot<U, B>>, key: &UpdateKey, current: &semver::Version, confirm: impl FnOnce(&str) -> Result<bool, String>, install: impl FnOnce(U, B) -> Result<(), String>) -> Result<InstallOutcome, InstallError> {
    let version = {
        let mut held = lock_slot(slot)?;
        if held.confirming {
            return Err(InstallError::kept("An update installation is already waiting for your answer."));
        }
        let version = held.downloaded_version(key)?;
        if !is_upgrade(current, &version) {
            return Err("The offered update is not newer than the installed version.".to_string().into());
        }
        held.confirming = true;
        version
    };
    let _confirming = ConfirmingGuard(slot);
    if !confirm(&version).map_err(InstallError::kept)? {
        return Ok(InstallOutcome::Cancelled);
    }
    let (update, bytes) = lock_slot(slot)?.take_for_install(key)?;
    install(update, bytes)?;
    Ok(InstallOutcome::Started)
}

pub fn confirmation_text(version: &str) -> String {
    format!("PDF Workstation {version} is ready to install.\n\nPDF Workstation will close and reopen to finish the update. Save your work first: changes that are not saved will be lost.\n\nInstall version {version} now?")
}

const CONFIRMATION_TITLE: &str = "Install update?";

#[cfg(windows)]
fn ask_to_install(owner: isize, version: &str) -> Result<bool, String> {
    use windows::{core::PCWSTR, Win32::{Foundation::HWND, UI::WindowsAndMessaging::{MessageBoxW, MB_DEFBUTTON2, MB_ICONWARNING, MB_SETFOREGROUND, MB_YESNO}}};
    let wide = |text: &str| text.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(&confirmation_text(version)), wide(CONFIRMATION_TITLE));
    let owner = HWND(owner as *mut _);
    let answer = unsafe { MessageBoxW(Some(owner), PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2 | MB_SETFOREGROUND) };
    read_answer(answer.0)
}

/// Reads a `MessageBoxW` result. 0 means the box could not be shown (for
/// example its owner window was destroyed): an error, not a No.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn read_answer(answer: i32) -> Result<bool, String> {
    const IDYES: i32 = 6;
    match answer {
        0 => Err("The update confirmation could not be shown. Nothing was installed. Close and reopen PDF Workstation, then try again.".into()),
        answer => Ok(answer == IDYES),
    }
}

#[cfg(not(windows))]
fn ask_to_install(_owner: (), version: &str) -> Result<bool, String> {
    Ok(rfd::MessageDialog::new().set_title(CONFIRMATION_TITLE).set_description(confirmation_text(version)).set_buttons(rfd::MessageButtons::YesNo).set_level(rfd::MessageLevel::Warning).show() == rfd::MessageDialogResult::Yes)
}

#[tauri::command]
pub async fn install_update<R: Runtime>(app: AppHandle<R>, update: UpdateKey) -> Result<InstallOutcome, InstallError> {
    #[cfg(windows)]
    let owner = require_owner(app.get_webview_window("main").and_then(|window| window.hwnd().ok()).map(|hwnd| hwnd.0 as isize))?;
    #[cfg(not(windows))]
    let owner = ();
    let worker = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let state = worker.state::<PendingUpdate>();
        install_confirmed(&state, &update, &worker.package_info().version, |version| ask_to_install(owner, version), |pending, bytes| pending.restart_after_install(true).install(&bytes).map_err(|error| error.to_string()))
    })
    .await
    .map_err(|error| InstallError::from(error.to_string()))?
}

#[tauri::command]
pub fn release_update(state: State<'_, PendingUpdate>, update: UpdateKey) -> Result<(), String> {
    lock(&state)?.release(&update);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(version: &str, signature: &str) -> UpdateKey {
        UpdateKey { version: version.into(), signature: signature.into(), check: 1 }
    }

    /// A slot whose first check (number 1) is in flight.
    fn fresh<U>() -> UpdateSlot<U, Vec<u8>> {
        let mut slot = UpdateSlot::default();
        assert_eq!(slot.begin_check(), Ok(1));
        slot
    }

    fn downloaded(k: &UpdateKey) -> UpdateSlot<&'static str, Vec<u8>> {
        let mut slot = fresh();
        slot.offer(k.clone(), "update").unwrap();
        let update = slot.begin_download(k).unwrap();
        assert!(slot.finish_download(k, update, Ok(vec![1, 2, 3])));
        slot
    }

    #[test]
    fn webview_key_rejects_endpoint_proxy_header_target_and_downgrade_fields() {
        assert_eq!(serde_json::from_value::<UpdateKey>(serde_json::json!({ "version": "0.3.0", "signature": "sig", "check": 1 })).unwrap(), key("0.3.0", "sig"));
        assert!(serde_json::from_value::<UpdateKey>(serde_json::json!({ "version": "0.3.0", "signature": "sig" })).is_err(), "the check number is required");
        for extra in ["proxy", "headers", "target", "allowDowngrades", "endpoints", "pubkey", "timeout", "restartAfterInstall", "rid"] {
            let mut value = serde_json::json!({ "version": "0.3.0", "signature": "sig", "check": 1 });
            value[extra] = serde_json::json!("http://attacker.invalid");
            assert!(serde_json::from_value::<UpdateKey>(value).is_err(), "{extra} must be rejected");
        }
    }

    #[test]
    fn install_rejects_without_a_prior_check_or_download() {
        let k = key("0.3.0", "sig");
        let mut slot: UpdateSlot<&str, Vec<u8>> = fresh();
        assert!(slot.take_for_install(&k).is_err());
        assert!(slot.begin_download(&k).is_err());
        slot.offer(k.clone(), "update").unwrap();
        assert!(slot.take_for_install(&k).is_err(), "a checked but undownloaded update must not install");
    }

    #[test]
    fn install_takes_the_checked_update_once() {
        let k = key("0.3.0", "sig");
        let mut slot = downloaded(&k);
        let (update, bytes) = slot.take_for_install(&k).unwrap();
        assert_eq!((update, bytes), ("update", vec![1, 2, 3]));
        assert!(slot.take_for_install(&k).is_err(), "a consumed update must not install twice");
        assert!(slot.begin_download(&k).is_err());
    }

    #[test]
    fn mismatched_version_or_signature_is_rejected_and_keeps_the_real_offer() {
        let k = key("0.3.0", "sig");
        let mut slot = downloaded(&k);
        assert!(slot.take_for_install(&key("0.3.0", "other")).is_err());
        assert!(slot.take_for_install(&key("0.4.0", "sig")).is_err());
        assert!(slot.take_for_install(&k).is_ok());
        let mut checked: UpdateSlot<&str, Vec<u8>> = fresh();
        checked.offer(k.clone(), "update").unwrap();
        assert!(checked.begin_download(&key("0.3.0", "forged")).is_err());
        assert!(checked.begin_download(&k).is_ok());
    }

    #[test]
    fn a_new_check_replaces_the_previous_offer() {
        let old = key("0.3.0", "old");
        let new = UpdateKey { check: 2, ..key("0.3.1", "new") };
        let mut slot = downloaded(&old);
        assert_eq!(slot.begin_check(), Ok(2));
        assert!(slot.begin_download(&old).is_err(), "starting a check clears the previous offer");
        slot.offer(new.clone(), "newer").unwrap();
        assert!(slot.begin_download(&old).is_err());
        assert_eq!(slot.begin_download(&new).unwrap(), "newer");
    }

    #[test]
    fn failed_download_keeps_the_offer_for_retry_and_concurrent_download_is_refused() {
        let k = key("0.3.0", "sig");
        let mut slot: UpdateSlot<&str, Vec<u8>> = fresh();
        slot.offer(k.clone(), "update").unwrap();
        let update = slot.begin_download(&k).unwrap();
        assert!(slot.begin_download(&k).is_err());
        assert!(slot.offer(key("0.3.1", "x"), "other").is_err());
        assert!(slot.take_for_install(&k).is_err());
        assert!(slot.finish_download(&k, update, Err(())));
        assert!(slot.take_for_install(&k).is_err());
        let update = slot.begin_download(&k).unwrap();
        assert!(slot.finish_download(&k, update, Ok(vec![9])));
        assert_eq!(slot.take_for_install(&k).unwrap().1, vec![9]);
    }

    #[test]
    fn release_during_download_discards_the_result() {
        let k = key("0.3.0", "sig");
        let mut slot: UpdateSlot<&str, Vec<u8>> = fresh();
        slot.offer(k.clone(), "update").unwrap();
        let update = slot.begin_download(&k).unwrap();
        assert!(slot.release(&k));
        assert!(!slot.finish_download(&k, update, Ok(vec![1])));
        assert!(slot.take_for_install(&k).is_err());
    }

    #[test]
    fn release_clears_only_the_named_offer() {
        let k = key("0.3.0", "sig");
        let mut slot = downloaded(&k);
        for other in [key("0.3.0", "other"), key("0.4.0", "sig"), UpdateKey { check: 2, ..k.clone() }] {
            assert!(!slot.release(&other));
        }
        assert_eq!(slot.take_for_install(&k).unwrap().0, "update", "a non-matching release keeps the offer");
        let mut empty: UpdateSlot<&str, Vec<u8>> = fresh();
        assert!(!empty.release(&k));
    }

    #[test]
    fn a_stale_check_cannot_replace_or_release_a_newer_checks_offer() {
        let mut slot: UpdateSlot<&str, Vec<u8>> = UpdateSlot::default();
        let closed = slot.begin_check().unwrap();
        let reopened = slot.begin_check().unwrap();
        let fresh_key = UpdateKey { version: "0.3.0".into(), signature: "sig".into(), check: reopened };
        let stale_key = UpdateKey { check: closed, ..fresh_key.clone() };
        slot.offer(fresh_key.clone(), "reopened").unwrap();
        assert!(slot.offer(stale_key.clone(), "closed").is_err(), "a check that finishes late must not replace the latest offer");
        assert!(!slot.release(&stale_key), "the closed dialog's release names the same release but a different check");
        assert_eq!(slot.begin_download(&fresh_key).unwrap(), "reopened");
    }

    fn current() -> semver::Version {
        semver::Version::parse("0.2.0").unwrap()
    }

    #[test]
    fn declining_the_confirmation_installs_nothing_and_keeps_the_downloaded_offer() {
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        let mut asked = Vec::new();
        let mut installed = false;
        let outcome = install_confirmed(&slot, &k, &current(), |version| { asked.push(version.to_string()); Ok(false) }, |_, _| { installed = true; Ok(()) });
        assert_eq!(outcome, Ok(InstallOutcome::Cancelled));
        assert_eq!(asked, ["0.3.0"]);
        assert!(!installed);
        assert_eq!(slot.lock().unwrap().take_for_install(&k).unwrap(), ("update", vec![1, 2, 3]), "No keeps the downloaded offer for a later Yes");
    }

    #[test]
    fn accepting_the_confirmation_installs_the_held_update_once() {
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        let mut installed = Vec::new();
        let outcome = install_confirmed(&slot, &k, &current(), |_| Ok(true), |update, bytes| { installed.push((update, bytes)); Ok(()) });
        assert_eq!(outcome, Ok(InstallOutcome::Started));
        assert_eq!(installed, [("update", vec![1, 2, 3])]);
        assert!(install_confirmed(&slot, &k, &current(), |_| Ok(true), |_, _| Ok(())).is_err(), "a consumed update must not install twice");
    }

    #[test]
    fn confirmation_is_not_shown_for_an_unheld_or_stale_update() {
        let k = key("0.3.0", "sig");
        let empty: Mutex<UpdateSlot<&str, Vec<u8>>> = Mutex::new(UpdateSlot::default());
        let mut asked = false;
        assert!(install_confirmed(&empty, &k, &current(), |_| { asked = true; Ok(true) }, |_, _| Ok(())).is_err());
        let slot = Mutex::new(downloaded(&k));
        assert!(install_confirmed(&slot, &key("0.9.0", "forged"), &current(), |_| { asked = true; Ok(true) }, |_, _| Ok(())).is_err());
        assert!(install_confirmed(&slot, &k, &semver::Version::parse("0.3.0").unwrap(), |_| { asked = true; Ok(true) }, |_, _| Ok(())).is_err());
        assert!(!asked, "the question must only name an update the backend downloaded and that is newer");
    }

    #[test]
    fn an_offer_replaced_while_the_question_is_open_is_not_installed() {
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        let mut installed = false;
        let outcome = install_confirmed(&slot, &k, &current(), |_| { slot.lock().unwrap().release(&k); Ok(true) }, |_, _| { installed = true; Ok(()) });
        assert!(outcome.is_err());
        assert!(!installed);
    }

    #[test]
    fn a_second_install_while_the_question_is_open_is_refused_without_asking() {
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        let mut second = None;
        let mut asked_twice = false;
        let mut installed = 0;
        let outcome = install_confirmed(&slot, &k, &current(), |_| {
            second = Some(install_confirmed(&slot, &k, &current(), |_| { asked_twice = true; Ok(true) }, |_, _| { installed += 1; Ok(()) }));
            Ok(false)
        }, |_, _| Ok(()));
        assert_eq!(outcome, Ok(InstallOutcome::Cancelled));
        assert!(second.unwrap().unwrap_err() == InstallError::kept("An update installation is already waiting for your answer."));
        assert!(!asked_twice, "only one confirmation may be shown at a time");
        assert_eq!(installed, 0);
        let mut asked_again = false;
        assert_eq!(install_confirmed(&slot, &k, &current(), |_| { asked_again = true; Ok(true) }, |_, _| Ok(())), Ok(InstallOutcome::Started), "answering the first question frees the next install");
        assert!(asked_again);
    }

    #[test]
    fn the_open_question_flag_clears_on_every_exit() {
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        assert!(install_confirmed(&slot, &k, &current(), |_| Ok(true), |_, _| Err("installer failed".into())).is_err());
        assert!(!slot.lock().unwrap().confirming, "a failed install must not leave the question marked open");
        let slot = Mutex::new(downloaded(&k));
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| install_confirmed(&slot, &k, &current(), |_| panic!("dialog failed"), |_, _| Ok(()))));
        assert!(panicked.is_err());
        assert!(!slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).confirming);
        let stale = Mutex::new(downloaded(&k));
        assert!(install_confirmed(&stale, &k, &semver::Version::parse("0.3.0").unwrap(), |_| Ok(true), |_, _| Ok(())).is_err());
        assert!(!stale.lock().unwrap().confirming, "a refused install never marks the question open");
    }

    #[test]
    fn install_is_refused_when_the_main_window_handle_is_missing() {
        assert_eq!(require_owner(Some(0x1234)), Ok(0x1234));
        for missing in [None, Some(0)] {
            let error = require_owner(missing).unwrap_err();
            assert!(error.message.contains("Nothing was installed"), "{missing:?} must refuse rather than show an ownerless box");
            assert!(error.offer_kept, "a refusal before asking keeps the downloaded offer");
        }
    }

    #[test]
    fn a_message_box_failure_is_an_error_not_a_decline() {
        assert!(read_answer(0).unwrap_err().contains("could not be shown"));
        assert_eq!(read_answer(6), Ok(true));
        assert_eq!(read_answer(7), Ok(false), "IDNO");
        #[cfg(windows)]
        assert_eq!(windows::Win32::UI::WindowsAndMessaging::IDYES.0, 6);
        let k = key("0.3.0", "sig");
        let slot = Mutex::new(downloaded(&k));
        let mut installed = false;
        let outcome = install_confirmed(&slot, &k, &current(), |_| read_answer(0), |_, _| { installed = true; Ok(()) });
        assert_eq!(outcome.unwrap_err().offer_kept, true);
        assert!(!installed);
        assert!(!slot.lock().unwrap().confirming);
        assert_eq!(slot.lock().unwrap().take_for_install(&k).unwrap().0, "update", "a box that failed to show keeps the downloaded offer");
    }

    #[test]
    fn install_errors_tell_the_webview_whether_the_offer_was_kept() {
        assert_eq!(serde_json::to_value(InstallError::kept("busy")).unwrap(), serde_json::json!({ "message": "busy", "offerKept": true }));
        let k = key("0.3.0", "sig");
        let empty: Mutex<UpdateSlot<&str, Vec<u8>>> = Mutex::new(fresh());
        assert!(!install_confirmed(&empty, &k, &current(), |_| Ok(true), |_, _| Ok(())).unwrap_err().offer_kept, "no held offer: nothing to keep");
        let slot = Mutex::new(downloaded(&k));
        assert!(!install_confirmed(&slot, &k, &current(), |_| Ok(true), |_, _| Err("installer failed".into())).unwrap_err().offer_kept, "Yes took the offer: it is gone");
    }

    #[test]
    fn confirmation_text_names_the_version_restart_and_unsaved_work() {
        let text = confirmation_text("0.3.0");
        assert!(text.contains("PDF Workstation 0.3.0 is ready to install."));
        assert!(text.contains("close and reopen"));
        assert!(text.contains("Save your work first"));
        assert!(text.contains("Install version 0.3.0 now?"));
        assert_eq!(serde_json::to_value(InstallOutcome::Cancelled).unwrap(), serde_json::json!("cancelled"));
        assert_eq!(serde_json::to_value(InstallOutcome::Started).unwrap(), serde_json::json!("started"));
    }

    #[test]
    fn only_strictly_newer_versions_are_upgrades() {
        let current = semver::Version::parse("0.2.12").unwrap();
        assert!(is_upgrade(&current, "0.2.13"));
        assert!(is_upgrade(&current, "0.3.0"));
        assert!(!is_upgrade(&current, "0.2.12"));
        assert!(!is_upgrade(&current, "0.2.11"));
        assert!(!is_upgrade(&current, "0.2.0"));
        assert!(!is_upgrade(&current, "not-a-version"));
        assert!(!is_upgrade(&current, ""));
    }
}
