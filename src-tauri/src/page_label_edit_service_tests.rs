use super::*;
use crate::outline_edit::tests::sample;
use crate::page_label_edit::LabelOp;
use crate::page_labels::{PageLabelStatus, Style};
use std::path::Path;

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/reportlab-page-labels.pdf");

fn direct<T>(service: &PdfService, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
    let (reply, receiver) = oneshot::channel();
    service.sender.send(request(reply)).map_err(|error| error.to_string())?;
    receiver.blocking_recv().map_err(|error| error.to_string())?
}
fn service() -> PdfService { PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll")) }
fn open_bytes(service: &PdfService, folder: &Path, name: &str, bytes: &[u8]) -> DocumentInfo {
    let path = folder.join(name); std::fs::write(&path, bytes).unwrap();
    direct(service, |reply| Request::Open(path.clone(), reply)).map(ReplyLease::accept).unwrap()
}
fn plain(service: &PdfService, folder: &Path, name: &str) -> DocumentInfo {
    let mut bytes = Vec::new(); sample().save_to(&mut bytes).unwrap();
    open_bytes(service, folder, name, &bytes)
}
fn view(service: &PdfService, info: &DocumentInfo) -> PageLabelEditResult { direct(service, |reply| Request::PageLabelEdit(info.id, info.revision, None, reply)).unwrap() }
fn edit(service: &PdfService, info: &DocumentInfo, version: u64, op: LabelOp) -> Result<PageLabelEditResult, String> {
    direct(service, |reply| Request::PageLabelEdit(info.id, info.revision, Some((version, op)), reply))
}
fn set(start_page: usize, style: Option<Style>, prefix: &str, first: u64) -> LabelOp { LabelOp::Set { start_page, style, prefix: prefix.into(), first } }
fn app_labels(service: &PdfService, info: &DocumentInfo) -> Vec<String> {
    let labels = direct(service, |reply| Request::PageLabels(info.id, info.revision, reply)).unwrap();
    match labels.status {
        PageLabelStatus::None => (1..=info.pages.len()).map(|n| n.to_string()).collect(),
        PageLabelStatus::Supported => labels.labels.into_iter().map(|label| label.label).collect(),
        PageLabelStatus::Unavailable => panic!("labels unavailable: {:?}", labels.reason),
    }
}
fn pdfium_labels(service: &PdfService, info: &DocumentInfo) -> Vec<String> {
    direct(service, |reply| Request::EnginePageLabels(info.id, reply)).unwrap().into_iter().enumerate().map(|(index, label)| label.unwrap_or_else(|| (index + 1).to_string())).collect()
}
fn lopdf_labels(path: &Path) -> Vec<String> {
    let document = lopdf::Document::load(path).unwrap();
    let count = document.get_pages().len();
    let ranges = crate::page_labels::read_ranges(&document, count).unwrap();
    if ranges.is_empty() { return (1..=count).map(|n| n.to_string()).collect(); }
    crate::page_labels::expand_ranges(&ranges, count).unwrap()
}
fn save(service: &PdfService, info: &DocumentInfo, pages: Option<Vec<usize>>, path: PathBuf) -> Result<SavedCopy, String> {
    direct(service, |reply| Request::Save(info.id, pages, path, reply))
}
fn strs(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
fn defined(service: &PdfService, info: &DocumentInfo) -> (PageLabelEditResult, Vec<String>) {
    let start = view(service, info);
    let a = edit(service, info, start.view.version, set(0, Some(Style::RomanLower), "", 1)).unwrap();
    let b = edit(service, info, a.view.version, set(2, Some(Style::Decimal), "", 5)).unwrap();
    let c = edit(service, info, b.view.version, set(4, Some(Style::AlphaUpper), "App \u{3a9}-", 2)).unwrap();
    (c, strs(&["i", "ii", "5", "6", "App \u{3a9}-B", "App \u{3a9}-C"]))
}

#[test]
fn edited_labels_save_into_a_new_copy_that_lopdf_pdfium_and_the_app_read_identically() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = plain(&service, folder.path(), "source.pdf");
    let source = std::fs::read(&info.path).unwrap();
    let start = view(&service, &info);
    assert!(start.view.editable && !start.view.changed && start.view.ranges.is_empty() && !start.document.dirty);
    assert_eq!(app_labels(&service, &info), strs(&["1", "2", "3", "4", "5", "6"]));
    let (edited, expected) = defined(&service, &info);
    assert!(edited.document.dirty && edited.view.changed, "a label edit marks the document as having unsaved changes");
    assert_eq!(edited.document.revision, info.revision, "label edits do not invalidate rendered pages");
    assert_eq!(app_labels(&service, &info), expected, "the page list shows the unsaved labels");

    assert!(save(&service, &info, None, PathBuf::from(&info.path)).err().unwrap().contains("new filename"));
    let copy_path = folder.path().join("labelled.pdf");
    let saved = save(&service, &info, None, copy_path.clone()).unwrap();
    assert!(saved.document.dirty, "Save a Copy keeps the session's unsaved state");
    assert_eq!(std::fs::read(&info.path).unwrap(), source, "the source file is never changed");
    assert_eq!(lopdf_labels(&copy_path), expected);
    let reopened = direct(&service, |reply| Request::Open(copy_path, reply)).map(ReplyLease::accept).unwrap();
    assert!(!reopened.dirty);
    assert_eq!(app_labels(&service, &reopened), expected, "the app's reader");
    assert_eq!(pdfium_labels(&service, &reopened), expected, "PDFium's reader, page by page");
    let again = view(&service, &reopened);
    assert!(again.view.editable && again.view.ranges.len() == 3 && !again.view.changed, "the written ranges are editable again");
}

#[test]
fn existing_labels_are_preserved_when_untouched_and_a_range_edit_changes_only_its_own_pages() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = open_bytes(&service, folder.path(), "labels.pdf", FIXTURE);
    let original = app_labels(&service, &info);
    assert_eq!(original, strs(&["1", "2", "iv", "v", "I", "II", "AA", "BB", "bb", "cc", "Appendix", "Appendix", "N-5", "N-6"]));
    let start = view(&service, &info);
    assert!(start.view.editable && !start.view.changed && start.view.ranges.len() >= 6);
    let untouched = folder.path().join("untouched.pdf");
    save(&service, &info, None, untouched.clone()).unwrap();
    assert_eq!(lopdf_labels(&untouched), original);

    let changed = edit(&service, &info, start.view.version, set(2, Some(Style::RomanLower), "", 3)).unwrap();
    let mut expected = original.clone(); expected[2] = "iii".into(); expected[3] = "iv".into();
    assert_eq!(app_labels(&service, &info), expected);
    let copy = folder.path().join("changed.pdf");
    save(&service, &info, None, copy.clone()).unwrap();
    assert_eq!(lopdf_labels(&copy), expected);
    let reopened = direct(&service, |reply| Request::Open(copy, reply)).map(ReplyLease::accept).unwrap();
    assert_eq!(pdfium_labels(&service, &reopened), expected);

    let deleted = edit(&service, &info, changed.view.version, LabelOp::Delete { start_page: 12 }).unwrap();
    assert_eq!(app_labels(&service, &info)[12..], strs(&["Appendix", "Appendix"])[..], "pages of a deleted range continue the one before");
    assert!(deleted.view.changed);
    let error = direct(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 2 }, reply)).err().unwrap();
    assert_eq!(error, crate::page_label_edit::PENDING_REFUSAL);
    let fresh = open_bytes(&service, folder.path(), "labels-again.pdf", FIXTURE);
    let error = direct(&service, |reply| Request::Edit(fresh.id, PageEdit::Move { from: 0, to: 2 }, reply)).err().unwrap();
    assert!(error.contains("page labels"), "structural edits on documents that already have labels stay refused: {error}");
}

#[test]
fn page_edits_and_label_edits_refuse_each_other_when_they_would_move_ranges() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = plain(&service, folder.path(), "interaction.pdf");
    let start = view(&service, &info);

    let moved = direct(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 3 }, reply)).unwrap();
    let blocked = direct(&service, |reply| Request::PageLabelEdit(info.id, moved.revision, None, reply)).unwrap();
    assert!(!blocked.view.editable && blocked.view.reason.as_deref() == Some(crate::page_label_edit::STRUCTURE_REFUSAL));
    let error = direct(&service, |reply| Request::PageLabelEdit(info.id, moved.revision, Some((blocked.view.version, set(0, Some(Style::Decimal), "", 9))), reply)).err().unwrap();
    assert_eq!(error, crate::page_label_edit::STRUCTURE_REFUSAL);
    let undone = direct(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
    let back = direct(&service, |reply| Request::PageLabelEdit(info.id, undone.revision, None, reply)).unwrap();
    assert!(back.view.editable && back.view.version == start.view.version);

    let (pending, expected) = {
        let a = direct(&service, |reply| Request::PageLabelEdit(info.id, undone.revision, Some((back.view.version, set(0, Some(Style::RomanUpper), "", 1))), reply)).unwrap();
        (a, ["I", "II", "III", "IV", "V", "VI"])
    };
    assert!(pending.document.dirty);
    for edit_request in [PageEdit::Move { from: 0, to: 1 }, PageEdit::Delete { pages: vec![5] }] {
        let error = direct(&service, |reply| Request::Edit(info.id, edit_request, reply)).err().unwrap();
        assert_eq!(error, crate::page_label_edit::PENDING_REFUSAL);
    }
    let redo = direct(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).err().unwrap();
    assert_eq!(redo, crate::page_label_edit::PENDING_REFUSAL, "redoing the earlier move would carry the ranges to other pages");
    let rotated = direct(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![1], clockwise: true }, reply)).unwrap();
    assert!(rotated.dirty);
    assert_eq!(app_labels(&service, &rotated), expected, "rotation neither refused nor moves labels");
    let still = direct(&service, |reply| Request::PageLabelEdit(info.id, rotated.revision, Some((pending.view.version, set(3, Some(Style::Decimal), "", 1))), reply)).unwrap();
    assert!(still.view.editable);
    let copy = folder.path().join("rotated.pdf");
    save(&service, &rotated, None, copy.clone()).unwrap();
    assert_eq!(lopdf_labels(&copy), strs(&["I", "II", "III", "1", "2", "3"]));
    let undo_rotate = direct(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
    assert!(undo_rotate.dirty, "label edits keep the document unsaved");
}

#[test]
fn extracting_and_splitting_a_labelled_copy_keeps_each_page_label() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = plain(&service, folder.path(), "extract.pdf");
    let (_, expected) = defined(&service, &info);
    let pick = vec![1usize, 2, 4];
    let extract = folder.path().join("extract-copy.pdf");
    let saved = save(&service, &info, Some(pick.clone()), extract.clone()).unwrap();
    assert_eq!(saved.document.pages.len(), 6);
    let want: Vec<String> = pick.iter().map(|page| expected[*page].clone()).collect();
    assert_eq!(lopdf_labels(&extract), want);
    let part = direct(&service, |reply| Request::Open(extract, reply)).map(ReplyLease::accept).unwrap();
    assert_eq!(pdfium_labels(&service, &part), want);

    let output = direct(&service, |reply| Request::Split(info.id, info.revision, 4, folder.path().join("parts"), reply)).unwrap();
    assert_eq!(output.files.len(), 2);
    assert_eq!(lopdf_labels(&output.files[0].path), expected[..4].to_vec());
    assert_eq!(lopdf_labels(&output.files[1].path), expected[4..].to_vec());
    let second = direct(&service, |reply| Request::Open(output.files[1].path.clone(), reply)).map(ReplyLease::accept).unwrap();
    assert_eq!(pdfium_labels(&service, &second), expected[4..].to_vec());
}

#[test]
fn signed_encrypted_and_hostile_documents_are_not_editable_and_say_why() {
    let bytes = |document: &mut lopdf::Document| { let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes };
    let mut signed = sample();
    signed.catalog_mut().unwrap().set("Perms", lopdf::Object::Dictionary(lopdf::dictionary! { "DocMDP" => lopdf::Object::Reference((9999, 0)) }));
    let mut session = EditSession::new(bytes(&mut signed), 6);
    let view = session.label_view();
    assert!(!view.editable && view.reason.as_deref().is_some_and(|reason| reason.contains("Signed")), "{:?}", view.reason);
    assert!(session.label_edit(view.version, set(0, Some(Style::Decimal), "", 2)).is_err() && !session.unsaved());

    let mut encrypted = sample();
    encrypted.trailer.set("Encrypt", lopdf::dictionary! { "Filter" => "Standard", "V" => 1, "R" => 2, "O" => lopdf::Object::string_literal(vec![0u8; 32]), "U" => lopdf::Object::string_literal(vec![0u8; 32]), "P" => -4 });
    let mut session = EditSession::new(bytes(&mut encrypted), 6);
    let view = session.label_view();
    assert!(!view.editable && view.reason.is_some());
    assert!(session.label_edit(view.version, set(0, Some(Style::Decimal), "", 2)).is_err());

    let mut cyclic = sample();
    let node = cyclic.new_object_id();
    cyclic.objects.insert(node, lopdf::Object::Dictionary(lopdf::dictionary! { "Kids" => vec![lopdf::Object::Reference(node)] }));
    cyclic.catalog_mut().unwrap().set("PageLabels", node);
    let mut session = EditSession::new(bytes(&mut cyclic), 6);
    let view = session.label_view();
    assert!(!view.editable && view.reason.as_deref().is_some_and(|reason| reason.contains("cycle")), "{:?}", view.reason);
    assert!(session.label_edit(view.version, set(0, Some(Style::Decimal), "", 2)).is_err() && !session.unsaved());
}

#[test]
fn label_only_edits_are_not_recovery_points_can_be_discarded_and_block_combining() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = plain(&service, folder.path(), "discard.pdf");
    let (_, expected) = defined(&service, &info);
    let root = folder.path().join("recovery");
    let error = direct(&service, |reply| Request::CheckpointRecovery(root, info.id, info.revision, 0, reply)).err().unwrap();
    assert!(error.contains("Page-label edits are not included in recovery points"), "{error}");
    assert!(direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision + 1, reply)).err().unwrap().contains("Discard again"));
    assert_eq!(app_labels(&service, &info), expected);
    direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision, reply)).expect("a label-only edit can be discarded");
    let after = view(&service, &info);
    assert!(!after.view.changed && !after.document.dirty && after.view.ranges.is_empty());
    assert_eq!(app_labels(&service, &info), strs(&["1", "2", "3", "4", "5", "6"]));
    assert!(direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision, reply)).err().unwrap().contains("Only unsaved edits"));

    let mut bytes = Vec::new(); sample().save_to(&mut bytes).unwrap();
    let mut edited = EditSession::new(bytes.clone(), 6);
    let version = edited.label_view().version;
    edited.label_edit(version, set(0, Some(Style::RomanLower), "", 1)).unwrap();
    let other = EditSession::new(bytes, 6);
    let error = crate::combine::validate_sources(&edited, &other).err().unwrap();
    assert!(error.contains("PageLabels"), "combining a session with pending labels is refused rather than dropping them: {error}");
    direct(&service, |reply| Request::Close(info.id, reply)).expect("close after a label-only edit");
}

#[test]
fn stale_versions_stale_revisions_and_failed_edits_change_nothing() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = plain(&service, folder.path(), "stale.pdf");
    let start = view(&service, &info);
    let first = edit(&service, &info, start.view.version, set(0, Some(Style::RomanLower), "", 1)).unwrap();
    assert!(edit(&service, &info, start.view.version, set(1, Some(Style::Decimal), "", 1)).err().unwrap().contains("changed"));
    assert!(edit(&service, &info, first.view.version, set(9, Some(Style::Decimal), "", 1)).err().unwrap().contains("out of range"));
    assert!(edit(&service, &info, first.view.version, LabelOp::Delete { start_page: 3 }).is_err());
    assert!(direct(&service, |reply| Request::PageLabelEdit(info.id, info.revision + 1, Some((first.view.version, LabelOp::Undo)), reply)).err().unwrap().contains("changed"));
    assert!(direct(&service, |reply| Request::PageLabelEdit(info.id + 1000, info.revision, None, reply)).err().unwrap().contains("closed"));
    assert_eq!(view(&service, &info).view.version, first.view.version);
    let undone = edit(&service, &info, first.view.version, LabelOp::Undo).unwrap();
    assert!(!undone.view.changed && !undone.document.dirty, "undoing every label edit leaves the document clean");
}

fn with_pending_label(service: &PdfService, folder: &Path, name: &str) -> DocumentInfo {
    let info = plain(service, folder, name);
    let start = view(service, &info);
    edit(service, &info, start.view.version, set(0, Some(Style::RomanLower), "", 1)).unwrap();
    info
}
const OVERLAY_REFUSAL: &str = "Save a copy with your bookmark and page-label edits first, or discard them.";
fn rect() -> Vec<crate::raster_redaction::RasterRedactionRect> { vec![crate::raster_redaction::RasterRedactionRect { x: 0.1, y: 0.1, width: 0.2, height: 0.2 }] }

#[test]
fn form_fill_refuses_while_label_edits_are_pending() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = with_pending_label(&service, folder.path(), "form.pdf");
    let error = direct(&service, |reply| Request::CheckFormCopy(info.id, info.revision, Vec::new(), reply)).unwrap_err();
    assert_eq!(error, OVERLAY_REFUSAL);
    let error = direct(&service, |reply| Request::FillFormCopy(info.id, info.revision, Vec::new(), folder.path().join("form-copy.pdf"), reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
}

#[test]
fn redaction_refuses_while_label_edits_are_pending() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = with_pending_label(&service, folder.path(), "redact.pdf");
    let error = direct(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision, 0, rect(), reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
    let target = folder.path().join("redacted.pdf");
    let error = direct(&service, |reply| Request::RasterRedact(info.id, info.revision, 0, rect(), target.clone(), reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
    assert!(!target.exists());
}

#[cfg(windows)]
#[test]
fn searchable_ocr_and_its_large_document_start_refuse_while_label_edits_are_pending() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = with_pending_label(&service, folder.path(), "ocr.pdf");
    let error = direct(&service, |reply| Request::PreflightSearchableOcr(info.id, info.revision, reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
    let error = direct(&service, |reply| Request::OcrJobSource(info.id, info.revision, reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
}

#[cfg(windows)]
#[test]
fn image_and_text_replacement_refuse_while_label_edits_are_pending() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = with_pending_label(&service, folder.path(), "replace.pdf");
    let error = direct(&service, |reply| Request::InspectImageReplacement(info.id, info.revision, 0, reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
    let error = direct(&service, |reply| Request::InspectTextReplacement(info.id, info.revision, 0, reply)).err().unwrap();
    assert_eq!(error, OVERLAY_REFUSAL);
}

#[test]
fn discarding_the_overlay_edits_lifts_the_refusal() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let info = with_pending_label(&service, folder.path(), "lifted.pdf");
    direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision, reply)).unwrap();
    let outcome = direct(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision, 0, rect(), reply));
    assert!(outcome.is_ok(), "{outcome:?}");
}
