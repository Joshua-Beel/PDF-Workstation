use super::*;
use crate::outline_edit::{tests::{sample, verify, with_outline, Node::{Closed, Item}}, BookmarkOp};
use std::path::Path;

fn direct<T>(service: &PdfService, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
    let (reply, receiver) = oneshot::channel();
    service.sender.send(request(reply)).map_err(|error| error.to_string())?;
    receiver.blocking_recv().map_err(|error| error.to_string())?
}
fn service() -> PdfService { PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll")) }
fn open(service: &PdfService, document: &mut lopdf::Document, folder: &Path, name: &str) -> (DocumentInfo, PathBuf, Vec<u8>) {
    let path = folder.join(name); document.save(&path).unwrap();
    let info = direct(service, |reply| Request::Open(path.clone(), reply)).map(ReplyLease::accept).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    (info, path, bytes)
}
fn outlined(folder: &Path, name: &str, service: &PdfService) -> (DocumentInfo, PathBuf, Vec<u8>) {
    let mut document = sample(); with_outline(&mut document, &tree()); open(service, &mut document, folder, name)
}
fn tree() -> Vec<crate::outline_edit::tests::Node> {
    vec![
        Item("A", Some(0), vec![Item("A1", Some(1), vec![]), Item("A2", Some(2), vec![Item("A2x", Some(2), vec![])])]),
        Closed("B", Some(3), vec![Item("B1", Some(4), vec![])]),
        Item("C", Some(5), vec![]),
    ]
}
fn edit(service: &PdfService, info: &DocumentInfo, version: u64, op: BookmarkOp) -> Result<BookmarkEditResult, String> {
    direct(service, |reply| Request::BookmarkEdit(info.id, info.revision, Some((version, op)), reply))
}
fn view(service: &PdfService, info: &DocumentInfo) -> BookmarkEditResult { direct(service, |reply| Request::BookmarkEdit(info.id, info.revision, None, reply)).unwrap() }
fn uid(result: &BookmarkEditResult, title: &str) -> u32 { result.view.entries.iter().find(|entry| entry.title == title).unwrap().uid }
type Row = (String, usize, Option<usize>);
fn rows(result: &BookmarkEditResult) -> Vec<Row> { result.view.entries.iter().map(|entry| (entry.title.clone(), entry.depth, entry.page)).collect() }
fn pdfium_rows(service: &PdfService, info: &DocumentInfo) -> Vec<Row> {
    let list = direct(service, |reply| Request::Bookmarks(info.id, info.revision, reply)).unwrap();
    assert!(!list.truncated);
    list.items.iter().map(|item| (item.title.clone(), item.depth, item.page)).collect()
}
fn lopdf_rows(document: &lopdf::Document) -> Vec<Row> {
    crate::outline_edit::read(document).unwrap().iter().map(|entry| (entry.title.clone(), entry.depth, entry.source_page)).collect()
}
fn save(service: &PdfService, info: &DocumentInfo, pages: Option<Vec<usize>>, path: PathBuf) -> Result<SavedCopy, String> {
    direct(service, |reply| Request::Save(info.id, pages, path, reply))
}
fn row(title: &str, depth: usize, page: usize) -> Row { (title.to_owned(), depth, Some(page)) }
fn reopen(service: &PdfService, path: PathBuf) -> DocumentInfo { direct(service, |reply| Request::Open(path, reply)).map(ReplyLease::accept).unwrap() }

#[test]
fn edited_bookmarks_save_into_a_new_copy_that_lopdf_and_pdfium_read_identically() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let (info, path, source) = outlined(folder.path(), "source.pdf", &service);
    let start = view(&service, &info);
    assert!(start.view.editable && !start.view.changed && !start.document.dirty);
    assert_eq!(rows(&start), vec![row("A", 0, 0), row("A1", 1, 1), row("A2", 1, 2), row("A2x", 2, 2), row("B", 0, 3), row("B1", 1, 4), row("C", 0, 5)]);
    assert!(!direct(&service, |reply| Request::Bookmarks(info.id, info.revision, reply)).unwrap().edited);

    let renamed = edit(&service, &info, start.view.version, BookmarkOp::Rename { uid: uid(&start, "A1"), title: "Intro".into() }).unwrap();
    assert!(renamed.document.dirty && renamed.view.changed, "a bookmark edit marks the document as having unsaved changes");
    assert_eq!(renamed.document.revision, info.revision, "bookmark edits do not invalidate rendered pages");
    let added = edit(&service, &info, renamed.view.version, BookmarkOp::Add { page: 3, title: "Added".into(), after: Some(uid(&renamed, "B")) }).unwrap();
    let indented = edit(&service, &info, added.view.version, BookmarkOp::Indent { uid: uid(&added, "C") }).unwrap();
    let deleted = edit(&service, &info, indented.view.version, BookmarkOp::Delete { uid: uid(&indented, "A2") }).unwrap();
    let moved = edit(&service, &info, deleted.view.version, BookmarkOp::MoveDown { uid: uid(&deleted, "A") }).unwrap();
    let expected = vec![row("B", 0, 3), row("B1", 1, 4), row("A", 0, 0), row("Intro", 1, 1), row("Added", 0, 3), row("C", 1, 5)];
    assert_eq!(rows(&moved), expected);
    let listing = direct(&service, |reply| Request::Bookmarks(info.id, info.revision, reply)).unwrap();
    assert!(listing.edited, "the read-only list flags that it shows the original bookmarks while edits are pending");
    assert_eq!(listing.items.len(), 7);

    assert!(save(&service, &info, None, path.clone()).err().unwrap().contains("new filename"), "the source is never overwritten");
    let copy_path = folder.path().join("edited.pdf");
    let saved = save(&service, &info, None, copy_path.clone()).unwrap();
    assert!(saved.document.dirty, "Save a Copy keeps the session's unsaved state");
    assert_eq!(std::fs::read(&path).unwrap(), source);

    let copy = lopdf::Document::load(&copy_path).unwrap();
    let written = verify(&copy);
    assert_eq!(written.iter().map(|(t, d, _)| (t.clone(), *d)).collect::<Vec<_>>(), expected.iter().map(|(t, d, _)| (t.clone(), *d)).collect::<Vec<_>>());
    assert_eq!(lopdf_rows(&copy), expected);
    let titles: Vec<String> = copy.objects.values().filter_map(|object| object.as_dict().ok()?.get(b"Title").ok().and_then(|t| lopdf::decode_text_string(t).ok())).collect();
    assert!(!titles.iter().any(|title| ["A1", "A2", "A2x"].contains(&title.as_str())), "deleted and renamed-away items are not carried into the copy: {titles:?}");
    let open_in_view: Vec<bool> = moved.view.entries.iter().enumerate().filter(|(index, entry)| moved.view.entries.get(index + 1).is_some_and(|next| next.depth > entry.depth)).map(|(_, entry)| entry.open).collect();
    let open_written: Vec<bool> = written.iter().enumerate().filter(|(index, entry)| written.get(index + 1).is_some_and(|next| next.1 > entry.1)).map(|(_, entry)| entry.2).collect();
    assert_eq!(open_written, open_in_view, "open and closed state survives the write (PDFium's list does not expose it, so it is read back from the copy)");
    assert_eq!(open_written, [false, true, true], "B stays closed; A and the added bookmark with a child stay open");
    let reopened = reopen(&service, copy_path);
    assert!(!reopened.dirty);
    assert_eq!(pdfium_rows(&service, &reopened), expected, "PDFium reads the written tree the same way");
}

#[test]
fn stale_versions_stale_revisions_and_failed_edits_change_nothing() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let (info, _, _) = outlined(folder.path(), "stale.pdf", &service);
    let start = view(&service, &info);
    let deleted = edit(&service, &info, start.view.version, BookmarkOp::Delete { uid: uid(&start, "C") }).unwrap();
    assert!(edit(&service, &info, start.view.version, BookmarkOp::Delete { uid: uid(&start, "B") }).err().unwrap().contains("changed"));
    assert!(edit(&service, &info, deleted.view.version, BookmarkOp::Indent { uid: uid(&deleted, "A") }).is_err());
    assert!(direct(&service, |reply| Request::BookmarkEdit(info.id, info.revision + 1, Some((deleted.view.version, BookmarkOp::Undo)), reply)).err().unwrap().contains("changed"));
    assert!(direct(&service, |reply| Request::BookmarkEdit(info.id + 1000, info.revision, None, reply)).err().unwrap().contains("closed"));
    assert_eq!(view(&service, &info).view.version, deleted.view.version);
    let undone = edit(&service, &info, deleted.view.version, BookmarkOp::Undo).unwrap();
    assert!(!undone.view.changed && !undone.document.dirty, "undoing every bookmark edit leaves the document clean");
    assert!(save(&service, &info, None, folder.path().join("unchanged.pdf")).is_ok());
}

#[test]
fn new_bookmarks_follow_page_moves_and_a_deleted_page_takes_its_bookmark_with_it() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let mut document = sample();
    let (info, _, _) = open(&service, &mut document, folder.path(), "plain.pdf");
    let start = view(&service, &info);
    assert!(start.view.editable && start.view.entries.is_empty());
    let first = edit(&service, &info, start.view.version, BookmarkOp::Add { page: 0, title: "First".into(), after: None }).unwrap();
    let third = edit(&service, &info, first.view.version, BookmarkOp::Add { page: 2, title: "Third".into(), after: None }).unwrap();
    let moved = direct(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 5 }, reply)).unwrap();
    let now = direct(&service, |reply| Request::BookmarkEdit(info.id, moved.revision, None, reply)).unwrap();
    assert_eq!(rows(&now), vec![row("First", 0, 5), row("Third", 0, 1)], "the panel shows the current page order");
    assert_eq!(now.view.version, third.view.version);
    let deleted = direct(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1] }, reply)).unwrap();
    let after = direct(&service, |reply| Request::BookmarkEdit(info.id, deleted.revision, None, reply)).unwrap();
    assert_eq!(rows(&after), vec![row("First", 0, 4), ("Third".to_owned(), 0, None)]);
    let copy_path = folder.path().join("moved.pdf");
    let saved = save(&service, &deleted, None, copy_path.clone()).unwrap();
    assert_eq!(saved.document.pages.len(), 5);
    let copy = lopdf::Document::load(&copy_path).unwrap();
    verify(&copy);
    assert_eq!(lopdf_rows(&copy), vec![row("First", 0, 4)]);
    let reopened = reopen(&service, copy_path);
    assert_eq!(pdfium_rows(&service, &reopened), vec![row("First", 0, 4)]);
    let selection = folder.path().join("selection.pdf");
    save(&service, &deleted, Some(vec![0, 1]), selection.clone()).unwrap();
    assert!(lopdf::Document::load(&selection).unwrap().catalog().unwrap().get(b"Outlines").is_err(), "extracting pages that hold no bookmark target carries no outline");
}

#[test]
fn an_existing_outline_still_blocks_page_deletion_and_follows_page_moves() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let (info, _, _) = outlined(folder.path(), "guarded.pdf", &service);
    let start = view(&service, &info);
    let renamed = edit(&service, &info, start.view.version, BookmarkOp::Rename { uid: uid(&start, "C"), title: "Closing".into() }).unwrap();
    let error = direct(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1] }, reply)).err().unwrap();
    assert!(error.contains("bookmarks"), "{error}");
    let moved = direct(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 2 }, reply)).unwrap();
    assert!(moved.dirty);
    let after = direct(&service, |reply| Request::BookmarkEdit(info.id, moved.revision, None, reply)).unwrap();
    assert!(after.view.changed && after.view.version == renamed.view.version);
    let copy_path = folder.path().join("reordered.pdf");
    save(&service, &moved, None, copy_path.clone()).unwrap();
    let copy = lopdf::Document::load(&copy_path).unwrap();
    verify(&copy);
    assert_eq!(lopdf_rows(&copy).last().unwrap().0, "Closing");
    let reopened = reopen(&service, copy_path);
    // Pages are now in source order [1, 2, 0, 3, 4, 5]; the untouched bookmarks still point at their own pages.
    let pages: Vec<_> = pdfium_rows(&service, &reopened).iter().map(|row| row.2).collect();
    assert_eq!(pages, vec![Some(2), Some(0), Some(1), Some(1), Some(3), Some(4), Some(5)]);
}

#[test]
fn bookmark_only_edits_are_not_part_of_recovery_points() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let (info, _, _) = outlined(folder.path(), "recovery.pdf", &service);
    let start = view(&service, &info);
    let changed = edit(&service, &info, start.view.version, BookmarkOp::Delete { uid: uid(&start, "C") }).unwrap();
    assert!(changed.document.dirty);
    let root = folder.path().join("recovery");
    let error = direct(&service, |reply| Request::CheckpointRecovery(root, info.id, info.revision, 0, reply)).err().unwrap();
    assert!(error.contains("Bookmark edits are not included in recovery points"), "{error}");
}

#[test]
fn combining_a_session_with_pending_bookmark_edits_is_refused_instead_of_dropping_them() {
    let mut bytes = Vec::new(); sample().save_to(&mut bytes).unwrap();
    let mut edited = EditSession::new(bytes.clone(), 6);
    let version = edited.bookmark_view().version;
    edited.bookmark_edit(version, BookmarkOp::Add { page: 0, title: "Start".into(), after: None }).unwrap();
    let other = EditSession::new(bytes, 6);
    let error = crate::combine::validate_sources(&edited, &other).err().unwrap();
    assert!(error.contains("Outlines"), "{error}");
}

#[test]
fn signed_cyclic_and_encrypted_documents_are_not_editable_and_say_why() {
    let bytes = |document: &mut lopdf::Document| { let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes };
    let mut signed = sample(); with_outline(&mut signed, &tree());
    signed.catalog_mut().unwrap().set("Perms", lopdf::Object::Dictionary(lopdf::dictionary! { "DocMDP" => lopdf::Object::Reference((9999, 0)) }));
    let mut session = EditSession::new(bytes(&mut signed), 6);
    let view = session.bookmark_view();
    assert!(!view.editable && view.reason.as_deref().is_some_and(|reason| reason.contains("Signed")), "{:?}", view.reason);
    assert!(session.bookmark_edit(view.version, BookmarkOp::Undo).is_err());
    assert!(!session.unsaved());

    let mut cyclic = sample(); with_outline(&mut cyclic, &tree());
    let title_id = |document: &lopdf::Document, name: &str| document.objects.keys().copied().find(|id| document.get_dictionary(*id).ok().and_then(|d| d.get(b"Title").ok()).and_then(|t| lopdf::decode_text_string(t).ok()).as_deref() == Some(name)).unwrap();
    let (last, first) = (title_id(&cyclic, "C"), title_id(&cyclic, "A"));
    cyclic.get_dictionary_mut(last).unwrap().set("Next", first);
    let mut session = EditSession::new(bytes(&mut cyclic), 6);
    let view = session.bookmark_view();
    assert!(!view.editable && view.reason.as_deref().is_some_and(|reason| reason.contains("cycle")));

    let mut encrypted = sample(); with_outline(&mut encrypted, &tree());
    encrypted.trailer.set("Encrypt", lopdf::dictionary! { "Filter" => "Standard", "V" => 1, "R" => 2, "O" => lopdf::Object::string_literal(vec![0u8; 32]), "U" => lopdf::Object::string_literal(vec![0u8; 32]), "P" => -4 });
    let mut session = EditSession::new(bytes(&mut encrypted), 6);
    assert!(!session.bookmark_view().editable, "encrypted documents cannot be edited");
}

#[test]
fn session_export_applies_pending_bookmarks_and_extraction_keeps_only_bookmarks_on_kept_pages() {
    let mut bytes = Vec::new(); sample().save_to(&mut bytes).unwrap();
    let mut session = EditSession::new(bytes.clone(), 6);
    let version = session.bookmark_view().version;
    session.bookmark_edit(version, BookmarkOp::Add { page: 1, title: "Second".into(), after: None }).unwrap();
    session.bookmark_edit(version + 1, BookmarkOp::Add { page: 4, title: "Fifth".into(), after: None }).unwrap();
    assert!(!session.dirty() && session.unsaved(), "bookmark edits are separate from the page plan");
    let full = lopdf::Document::load_mem(&session.export(None).unwrap()).unwrap();
    verify(&full);
    assert_eq!(lopdf_rows(&full), vec![row("Second", 0, 1), row("Fifth", 0, 4)]);
    let extracted = lopdf::Document::load_mem(&session.export(Some(&[1, 2])).unwrap()).unwrap();
    verify(&extracted);
    assert_eq!(lopdf_rows(&extracted), vec![row("Second", 0, 0)], "the bookmark on a page that was not extracted is left out");
    let untouched = EditSession::new(bytes, 6);
    assert!(lopdf::Document::load_mem(&untouched.export(None).unwrap()).unwrap().catalog().unwrap().get(b"Outlines").is_err());
}

#[test]
fn closing_or_discarding_after_a_bookmark_only_edit_works_and_drops_the_edit() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let (info, _, _) = outlined(folder.path(), "discard.pdf", &service);
    let start = view(&service, &info);
    let changed = edit(&service, &info, start.view.version, BookmarkOp::Delete { uid: uid(&start, "C") }).unwrap();
    assert!(changed.document.dirty);
    assert!(direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision + 1, reply)).err().unwrap().contains("Discard again"));
    direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision, reply)).expect("a bookmark-only edit can be discarded");
    let after = view(&service, &info);
    assert!(!after.view.changed && !after.document.dirty);
    assert_eq!(rows(&after), rows(&start), "the discarded edit is gone");
    assert!(direct(&service, |reply| Request::DiscardRecovery(info.id, info.revision, reply)).err().unwrap().contains("Only unsaved edits"), "a clean document still has nothing to discard");
    let again = edit(&service, &info, after.view.version, BookmarkOp::Delete { uid: uid(&after, "C") }).unwrap();
    assert!(again.document.dirty);
    direct(&service, |reply| Request::Close(info.id, reply)).expect("close after a bookmark-only edit");
}

#[test]
fn split_writes_added_bookmarks_into_the_part_that_holds_their_page() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let mut document = sample();
    let (info, _, _) = open(&service, &mut document, folder.path(), "split.pdf");
    let start = view(&service, &info);
    let one = edit(&service, &info, start.view.version, BookmarkOp::Add { page: 0, title: "One".into(), after: None }).unwrap();
    let two = edit(&service, &info, one.view.version, BookmarkOp::Add { page: 4, title: "Two".into(), after: None }).unwrap();
    let added = edit(&service, &info, two.view.version, BookmarkOp::Add { page: 5, title: "Three".into(), after: None }).unwrap();
    let output = direct(&service, |reply| Request::Split(info.id, info.revision, 3, folder.path().join("parts"), reply)).unwrap();
    assert_eq!(output.files.len(), 2);
    let first = lopdf_rows(&lopdf::Document::load(&output.files[0].path).unwrap());
    let second = lopdf::Document::load(&output.files[1].path).unwrap();
    verify(&second);
    let titles = |rows: &[Row]| rows.iter().map(|row| row.0.clone()).collect::<Vec<_>>();
    assert_eq!(titles(&first), ["One"]);
    assert_eq!(titles(&lopdf_rows(&second)), ["Two", "Three"]);
    assert!(added.view.changed);
}
