use serde::Serialize;
use std::{path::{Path, PathBuf}, sync::atomic::{AtomicBool, Ordering}};
use tauri::{Manager, State};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryCheckpointReceipt { document_id: u64, revision: u64, current_page: u32 }

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryDiscardReceipt { document_id: u64, revision: u64 }

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum MutationCommandError {
    Message(String),
    RecoveryCommitted { code: &'static str, #[serde(rename = "documentId")] document_id: u64, #[serde(rename = "requestedRevision")] requested_revision: u64, #[serde(rename = "committedRevision")] committed_revision: u64 },
}

impl From<String> for MutationCommandError {
    fn from(message: String) -> Self {
        let mut parts = message.split(':');
        if parts.next() == Some("RECOVERY_COMMITTED_REOPEN_REQUIRED") {
            if let (Some(id), Some(requested), Some(committed), None) = (parts.next(), parts.next(), parts.next(), parts.next()) {
                if let (Ok(document_id), Ok(requested_revision), Ok(committed_revision)) = (id.parse(), requested.parse(), committed.parse()) {
                    return Self::RecoveryCommitted { code: "recoveryCommittedReopenRequired", document_id, requested_revision, committed_revision };
                }
            }
        }
        Self::Message(message)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryRestoreResult { document: crate::service::DocumentInfo, current_page: u32 }

#[derive(Default)]
pub(crate) struct RecoveryCommands { restore_active: AtomicBool }

struct RestoreAttempt<'a> { commands: &'a RecoveryCommands }

impl RecoveryCommands {
    fn begin_restore(&self) -> Result<RestoreAttempt<'_>, String> {
        self.restore_active.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| "Finish the current recovery restore first.".to_owned())?;
        Ok(RestoreAttempt { commands: self })
    }
}

impl Drop for RestoreAttempt<'_> {
    fn drop(&mut self) { self.commands.restore_active.store(false, Ordering::Release); }
}

fn recovery_root(app_data: &Path) -> Result<PathBuf, String> {
    if !app_data.is_absolute() { return Err("The recovery folder is unavailable.".into()); }
    let root = app_data.join("recovery");
    std::fs::create_dir_all(&root).map_err(|_| "The recovery folder is unavailable.".to_owned())?;
    Ok(root)
}

#[tauri::command]
pub(crate) async fn checkpoint_recovery(app: tauri::AppHandle, service: State<'_, crate::service::PdfService>, id: u64, revision: u64, current_page: u32) -> Result<RecoveryCheckpointReceipt, MutationCommandError> {
    let app_data = crate::data_dirs::app_data_dir(&app).map_err(|_| MutationCommandError::from("The recovery folder is unavailable.".to_owned()))?;
    let root = recovery_root(&app_data).map_err(MutationCommandError::from)?;
    service.checkpoint_recovery(root, id, revision, current_page).await.map_err(MutationCommandError::from)?;
    Ok(RecoveryCheckpointReceipt { document_id: id, revision, current_page })
}

#[tauri::command]
pub(crate) async fn restore_recovery(app: tauri::AppHandle, service: State<'_, crate::service::PdfService>, commands: State<'_, RecoveryCommands>) -> Result<Option<RecoveryRestoreResult>, String> {
    let _attempt = commands.begin_restore()?;
    let root = recovery_root(&crate::data_dirs::app_data_dir(&app).map_err(|_| "The recovery folder is unavailable.")?)?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Restore unsaved PDF edits").add_filter("PDF documents", &["pdf"]).pick_file()).await.map_err(|error| error.to_string())?;
    let Some(path) = path else { return Ok(None); };
    let path = crate::open_path::gate_open_path_from(path, crate::open_path::OpenOrigin::UserChosen).await?;
    service.restore_recovery(root, path).await.map(|result| result.map(|recovered| RecoveryRestoreResult { document: recovered.document, current_page: recovered.current_page }))
}

#[tauri::command]
pub(crate) async fn keep_recovered_edits(service: State<'_, crate::service::PdfService>, id: u64, revision: u64) -> Result<RecoveryRestoreResult, String> {
    service.keep_recovered_edits(id, revision).await.map(|recovered| RecoveryRestoreResult { document: recovered.document, current_page: recovered.current_page })
}

#[tauri::command]
pub(crate) async fn open_original(service: State<'_, crate::service::PdfService>, id: u64, revision: u64) -> Result<crate::service::DocumentInfo, MutationCommandError> {
    service.open_original(id, revision).await.map_err(MutationCommandError::from)
}

#[tauri::command]
pub(crate) async fn discard_recovery(service: State<'_, crate::service::PdfService>, id: u64, revision: u64) -> Result<RecoveryDiscardReceipt, MutationCommandError> {
    service.discard_recovery(id, revision).await.map_err(MutationCommandError::from)?;
    Ok(RecoveryDiscardReceipt { document_id: id, revision })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_receipt_is_exact_and_contains_no_storage_identity() {
        let value = serde_json::to_value(RecoveryCheckpointReceipt { document_id: 4, revision: 7, current_page: 2 }).unwrap();
        assert_eq!(value, serde_json::json!({ "documentId": 4, "revision": 7, "currentPage": 2 }));
    }

    #[test]
    fn discard_receipt_is_exact_and_contains_no_storage_identity() {
        let value = serde_json::to_value(RecoveryDiscardReceipt { document_id: 4, revision: 7 }).unwrap();
        assert_eq!(value, serde_json::json!({ "documentId": 4, "revision": 7 }));
    }

    #[test]
    fn committed_recovery_error_is_a_strict_typed_boundary() {
        let value = serde_json::to_value(MutationCommandError::from("RECOVERY_COMMITTED_REOPEN_REQUIRED:4:7:8".to_owned())).unwrap();
        assert_eq!(value, serde_json::json!({ "code": "recoveryCommittedReopenRequired", "documentId": 4, "requestedRevision": 7, "committedRevision": 8 }));
        assert_eq!(serde_json::to_value(MutationCommandError::from("RECOVERY_COMMITTED_REOPEN_REQUIRED:4:7:8:extra".to_owned())).unwrap(), serde_json::json!("RECOVERY_COMMITTED_REOPEN_REQUIRED:4:7:8:extra"));
    }

    #[test]
    fn recovery_root_is_app_owned_absolute_and_created() {
        let parent = tempfile::tempdir().unwrap();
        let root = recovery_root(parent.path()).unwrap();
        assert_eq!(root, parent.path().join("recovery"));
        assert!(root.is_dir());
        assert!(recovery_root(Path::new("relative")).unwrap_err().contains("unavailable"));
    }

    #[test]
    fn restore_attempt_is_single_flight_and_drop_releases_it() {
        let commands = RecoveryCommands::default();
        let attempt = commands.begin_restore().unwrap();
        assert!(commands.begin_restore().err().unwrap().contains("current"));
        drop(attempt);
        drop(commands.begin_restore().unwrap());
        assert!(commands.begin_restore().is_ok());
    }
}
