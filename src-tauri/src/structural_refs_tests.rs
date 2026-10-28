//! Delete, move and extract on documents with bookmarks, named destinations and page labels (SM2).
//! Every copy is reloaded with lopdf and with PDFium and both readings are compared.
use super::*;
use crate::outline_edit::{tests::{sample, verify}, BookmarkOp};
use lopdf::{dictionary, Object, ObjectId};
use std::path::Path;

fn direct<T>(service: &PdfService, request: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
    let (reply, receiver) = oneshot::channel();
    service.sender.send(request(reply)).map_err(|error| error.to_string())?;
    receiver.blocking_recv().map_err(|error| error.to_string())?
}
fn service() -> PdfService { PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll")) }
fn open(service: &PdfService, path: &Path) -> DocumentInfo { direct(service, |reply| Request::Open(path.to_path_buf(), reply)).map(ReplyLease::accept).unwrap() }
fn save_bytes(document: &mut lopdf::Document) -> Vec<u8> { let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes }

enum Target { Dest(Object), Action(Object), External }
struct Node { title: &'static str, target: Target, closed: bool, kids: Vec<Node> }
fn node(title: &'static str, target: Target, kids: Vec<Node>) -> Node { Node { title, target, closed: false, kids } }
fn fit(page: ObjectId) -> Object { Object::Array(vec![Object::Reference(page), Object::Name(b"Fit".to_vec())]) }
fn string(value: &str) -> Object { Object::string_literal(value) }

fn add_outline(document: &mut lopdf::Document, nodes: Vec<Node>) {
    fn level(document: &mut lopdf::Document, parent: ObjectId, nodes: Vec<Node>) -> (Option<ObjectId>, Option<ObjectId>, i64) {
        let ids: Vec<ObjectId> = nodes.iter().map(|_| document.new_object_id()).collect();
        let mut visible = 0;
        let count = ids.len();
        for (index, node) in nodes.into_iter().enumerate() {
            let mut dict = dictionary! { "Title" => string(node.title), "Parent" => parent };
            match node.target {
                Target::Dest(dest) => dict.set("Dest", dest),
                Target::Action(action) => dict.set("A", action),
                Target::External => dict.set("A", dictionary! { "S" => "URI", "URI" => string("https://example.invalid/") }),
            }
            if index > 0 { dict.set("Prev", ids[index - 1]); }
            if index + 1 < count { dict.set("Next", ids[index + 1]); }
            let closed = node.closed;
            let (first, last, below) = level(document, ids[index], node.kids);
            if let (Some(first), Some(last)) = (first, last) {
                dict.set("First", first); dict.set("Last", last); dict.set("Count", if closed { -below } else { below });
                visible += 1 + if closed { 0 } else { below };
            } else { visible += 1; }
            document.objects.insert(ids[index], Object::Dictionary(dict));
        }
        (ids.first().copied(), ids.last().copied(), visible)
    }
    let root = document.new_object_id();
    let (first, last, count) = level(document, root, nodes);
    document.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => first.unwrap(), "Last" => last.unwrap(), "Count" => count }));
    document.catalog_mut().unwrap().set("Outlines", root);
}

/// Six pages: nested bookmarks using a direct /Dest, an inline and an indirect GoTo, a /Dests name, name-tree
/// strings and an external action; a two-leaf name tree with /Limits; labels i, ii, P-1, P-2, P-3, A.
fn fixture() -> lopdf::Document {
    let mut document = sample();
    let p: Vec<ObjectId> = document.get_pages().values().copied().collect();
    assert_eq!(p.len(), 6);
    let tree_p1 = document.add_object(fit(p[1]));
    let goto_b = document.add_object(dictionary! { "S" => "GoTo", "D" => string("tree-p1") });
    let mut d = node("D", Target::Dest(fit(p[5])), vec![node("D1", Target::Dest(fit(p[3])), vec![])]);
    d.closed = true;
    add_outline(&mut document, vec![
        node("A", Target::Dest(fit(p[0])), vec![
            node("A1", Target::Action(Object::Dictionary(dictionary! { "S" => "GoTo", "D" => vec![Object::Reference(p[1]), "XYZ".into(), Object::Null, Object::Null, Object::Null] })), vec![
                node("A1a", Target::Dest(fit(p[2])), vec![]),
            ]),
            node("A2", Target::Dest(Object::Name(b"Named-p3".to_vec())), vec![]),
        ]),
        node("B", Target::Action(Object::Reference(goto_b)), vec![
            node("B1", Target::Dest(fit(p[4])), vec![]),
            node("B2", Target::Dest(string("tree-p5")), vec![]),
        ]),
        node("C", Target::External, vec![]),
        d,
    ]);
    let dests = document.add_object(dictionary! { "Named-p3" => fit(p[3]), "Named-p1" => dictionary! { "D" => fit(p[1]) } });
    let leaf1 = document.add_object(dictionary! { "Limits" => vec![string("tree-p1"), string("tree-p2")], "Names" => vec![string("tree-p1"), Object::Reference(tree_p1), string("tree-p2"), fit(p[2])] });
    let leaf2 = document.add_object(dictionary! { "Limits" => vec![string("tree-p5"), string("tree-p5")], "Names" => vec![string("tree-p5"), fit(p[5])] });
    let root = document.add_object(dictionary! { "Kids" => vec![Object::Reference(leaf1), Object::Reference(leaf2)] });
    let labels = document.add_object(dictionary! { "Nums" => vec![
        0.into(), dictionary! { "S" => "r" }.into(),
        2.into(), dictionary! { "S" => "D", "P" => string("P-") }.into(),
        5.into(), dictionary! { "S" => "A" }.into(),
    ] });
    let catalog = document.catalog_mut().unwrap();
    catalog.set("Dests", dests);
    catalog.set("Names", dictionary! { "Dests" => root });
    catalog.set("PageLabels", labels);
    document
}

type Row = (String, usize, Option<usize>);
fn row(title: &str, depth: usize, page: Option<usize>) -> Row { (title.to_owned(), depth, page) }
fn lopdf_rows(document: &lopdf::Document) -> Vec<Row> {
    crate::outline_edit::read(document).unwrap().iter().map(|entry| (entry.title.clone(), entry.depth, entry.source_page)).collect()
}
fn pdfium_rows(service: &PdfService, info: &DocumentInfo) -> Vec<Row> {
    let list = direct(service, |reply| Request::Bookmarks(info.id, info.revision, reply)).unwrap();
    assert!(!list.truncated);
    list.items.iter().map(|item| (item.title.clone(), item.depth, item.page)).collect()
}
fn lopdf_labels(document: &lopdf::Document) -> Vec<String> {
    let count = document.get_pages().len();
    crate::page_labels::expand_ranges(&crate::page_labels::read_ranges(document, count).unwrap(), count).unwrap()
}
fn pdfium_labels(service: &PdfService, info: &DocumentInfo) -> Vec<String> {
    direct(service, |reply| Request::EnginePageLabels(info.id, reply)).unwrap().into_iter().map(Option::unwrap).collect()
}
/// Named destinations of a copy: name to zero-based page index (None when the target is not a page).
fn names(document: &lopdf::Document) -> Vec<(String, Option<usize>)> {
    let pages: HashMap<ObjectId, usize> = document.get_pages().values().enumerate().map(|(index, id)| (*id, index)).collect();
    let mut names: Vec<_> = crate::named_dests::read(document).unwrap().targets(document).into_iter().map(|(name, target)| (String::from_utf8(name).unwrap(), target.and_then(|id| pages.get(&id).copied()))).collect();
    names.sort();
    names
}
fn has_title(document: &lopdf::Document, title: &str) -> bool {
    document.objects.values().any(|object| object.as_dict().ok().and_then(|dict| dict.get(b"Title").ok()).and_then(|value| lopdf::decode_text_string(value).ok()).as_deref() == Some(title))
}

/// Saves `info` to `name`, reloads it with lopdf and PDFium, checks both agree, and returns both readings.
fn saved(service: &PdfService, info: &DocumentInfo, folder: &Path, name: &str, pages: Option<Vec<usize>>) -> (lopdf::Document, Vec<Row>, Vec<String>) {
    let path = folder.join(name);
    direct(service, |reply| Request::Save(info.id, pages, path.clone(), reply)).unwrap();
    let copy = lopdf::Document::load(&path).unwrap();
    verify(&copy);
    let rows = lopdf_rows(&copy);
    let labels = lopdf_labels(&copy);
    let reopened = open(service, &path);
    assert_eq!(pdfium_rows(service, &reopened), rows, "PDFium and lopdf must read the same bookmarks in {name}");
    assert_eq!(pdfium_labels(service, &reopened), labels, "PDFium and lopdf must read the same page labels in {name}");
    (copy, rows, labels)
}

#[test]
fn delete_move_and_extract_keep_bookmarks_named_destinations_and_labels_on_their_pages() {
    let service = service(); let folder = tempfile::tempdir().unwrap();
    let source = folder.path().join("source.pdf");
    fixture().save(&source).unwrap();
    let source_bytes = std::fs::read(&source).unwrap();
    let info = open(&service, &source);
    let original = lopdf::Document::load(&source).unwrap();
    let all = vec![row("A", 0, Some(0)), row("A1", 1, Some(1)), row("A1a", 2, Some(2)), row("A2", 1, Some(3)), row("B", 0, Some(1)), row("B1", 1, Some(4)), row("B2", 1, Some(5)), row("C", 0, None), row("D", 0, Some(5)), row("D1", 1, Some(3))];
    assert_eq!(lopdf_rows(&original), all, "the reader resolves direct, GoTo, /Dests and name-tree destinations");
    assert_eq!(pdfium_rows(&service, &info), all);
    assert_eq!(pdfium_labels(&service, &info), ["i", "ii", "P-1", "P-2", "P-3", "A"]);

    let deleted = direct(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1, 4] }, reply)).unwrap();
    let (copy, rows, labels) = saved(&service, &deleted, folder.path(), "deleted.pdf", None);
    assert_eq!(rows, vec![row("A", 0, Some(0)), row("A1a", 1, Some(1)), row("A2", 1, Some(2)), row("B2", 0, Some(3)), row("C", 0, None), row("D", 0, Some(3)), row("D1", 1, Some(2))],
        "bookmarks to removed pages are dropped and their children promoted; the rest keep their pages");
    assert_eq!(verify(&copy).iter().map(|(title, _, open)| (title.as_str(), *open)).filter(|(title, _)| *title == "D").collect::<Vec<_>>(), [("D", false)], "a closed bookmark stays closed");
    assert_eq!(labels, ["i", "P-1", "P-2", "A"], "each kept page keeps its original label");
    assert_eq!(names(&copy), vec![("Named-p3".into(), Some(2)), ("tree-p2".into(), Some(1)), ("tree-p5".into(), Some(3))], "named destinations to removed pages are dropped");
    assert_eq!(copy.get_pages().len(), 4);
    for title in ["A1", "B", "B1"] { assert!(!has_title(&copy, title), "the dropped bookmark {title} is pruned from the copy"); }
    assert_eq!(copy.objects.values().filter(|object| object.as_dict().is_ok_and(|dict| dict.get(b"Type").and_then(Object::as_name).is_ok_and(|kind| kind == b"Page"))).count(), 4, "removed pages are pruned");
    assert!(!copy.objects.values().any(|object| object.as_array().is_ok_and(|array| matches!(array.first(), Some(Object::Null)))), "no destination is left pointing at null");

    let moved = direct(&service, |reply| Request::Edit(deleted.id, PageEdit::Undo, reply)).unwrap();
    let moved = { assert!(!moved.dirty); direct(&service, |reply| Request::Edit(moved.id, PageEdit::Move { from: 0, to: 5 }, reply)).unwrap() };
    let (_, rows, labels) = saved(&service, &moved, folder.path(), "moved.pdf", None);
    assert_eq!(rows, vec![row("A", 0, Some(5)), row("A1", 1, Some(0)), row("A1a", 2, Some(1)), row("A2", 1, Some(2)), row("B", 0, Some(0)), row("B1", 1, Some(3)), row("B2", 1, Some(4)), row("C", 0, None), row("D", 0, Some(4)), row("D1", 1, Some(2))]);
    assert_eq!(labels, ["ii", "P-1", "P-2", "P-3", "A", "i"], "a moved page keeps its label");

    let fresh = open(&service, &source);
    let (copy, rows, labels) = saved(&service, &fresh, folder.path(), "extracted.pdf", Some(vec![2, 3]));
    assert_eq!(rows, vec![row("A1a", 0, Some(0)), row("A2", 0, Some(1)), row("C", 0, None), row("D1", 0, Some(1))]);
    assert_eq!(labels, ["P-1", "P-2"]);
    assert_eq!(names(&copy), vec![("Named-p3".into(), Some(1)), ("tree-p2".into(), Some(0))]);
    assert_eq!(std::fs::read(&source).unwrap(), source_bytes, "the source file is untouched");
}

#[test]
fn pending_bookmark_edits_compose_with_page_deletion_and_pending_label_edits_still_block_it() {
    let mut document = fixture();
    let mut session = EditSession::new(save_bytes(&mut document), 6);
    let view = session.bookmark_view();
    let a2 = view.entries.iter().find(|entry| entry.title == "A2").unwrap().uid;
    session.bookmark_edit(view.version, BookmarkOp::Rename { uid: a2, title: "Renamed".into() }).unwrap();
    session.bookmark_edit(view.version + 1, BookmarkOp::Add { page: 4, title: "Added on 5".into(), after: None }).unwrap();
    session.apply(PageEdit::Delete { pages: vec![1, 4] }).unwrap();
    let copy = lopdf::Document::load_mem(&session.export(None).unwrap()).unwrap();
    verify(&copy);
    assert_eq!(lopdf_rows(&copy), vec![row("A", 0, Some(0)), row("A1a", 1, Some(1)), row("Renamed", 1, Some(2)), row("B2", 0, Some(3)), row("C", 0, None), row("D", 0, Some(3)), row("D1", 1, Some(2))]);
    assert_eq!(lopdf_labels(&copy), ["i", "P-1", "P-2", "A"]);

    let mut labelled = EditSession::new(save_bytes(&mut fixture()), 6);
    let version = labelled.label_view().version;
    labelled.label_edit(version, crate::page_label_edit::LabelOp::Delete { start_page: 5 }).unwrap();
    assert_eq!(labelled.apply(PageEdit::Delete { pages: vec![1] }).unwrap_err(), crate::page_label_edit::PENDING_REFUSAL);
    let extracted = lopdf::Document::load_mem(&labelled.export(Some(&[1, 5])).unwrap()).unwrap();
    assert_eq!(lopdf_labels(&extracted), ["ii", "P-4"], "extraction still applies pending label edits to the kept pages");
}

#[test]
fn hostile_or_unsupported_references_still_refuse_page_removal_and_leave_the_session_unchanged() {
    let refused = |document: &mut lopdf::Document, edit: PageEdit, expected: &str| {
        let bytes = save_bytes(document);
        let mut session = EditSession::new(bytes.clone(), 6);
        let error = session.apply(edit).unwrap_err();
        assert!(error.contains(expected), "{error}");
        assert_eq!(session.revision, 0); assert!(!session.can_undo()); assert_eq!(session.source, bytes);
        assert!(session.export(Some(&[0, 2])).unwrap_err().contains(expected), "extraction refuses for the same reason");
    };
    let delete = || PageEdit::Delete { pages: vec![1] };
    let title = |document: &lopdf::Document, name: &str| document.objects.keys().copied().find(|id| document.get_dictionary(*id).ok().and_then(|d| d.get(b"Title").ok()).and_then(|t| lopdf::decode_text_string(t).ok()).as_deref() == Some(name)).unwrap();

    let mut aliased = fixture();
    let item = title(&aliased, "A1");
    let info = aliased.add_object(dictionary! { "Producer" => string("x"), "Alias" => item });
    aliased.trailer.set("Info", info);
    refused(&mut aliased, delete(), "also used by something else");

    let mut shared = fixture();
    let (a1, d) = (title(&shared, "A1"), title(&shared, "D"));
    shared.get_dictionary_mut(a1).unwrap().set("First", d);
    shared.get_dictionary_mut(a1).unwrap().set("Last", d);
    shared.get_dictionary_mut(d).unwrap().remove(b"Parent");
    refused(&mut shared, delete(), "shared item");

    let mut page_node = fixture();
    let page = page_node.get_pages()[&2];
    let root = page_node.catalog().unwrap().get(b"Names").unwrap().as_dict().unwrap().get(b"Dests").unwrap().as_reference().unwrap();
    page_node.get_dictionary_mut(root).unwrap().set("Kids", vec![Object::Reference(page)]);
    refused(&mut page_node, delete(), crate::named_dests::REFUSAL);

    let mut cyclic = fixture();
    let root = cyclic.catalog().unwrap().get(b"Names").unwrap().as_dict().unwrap().get(b"Dests").unwrap().as_reference().unwrap();
    cyclic.get_dictionary_mut(root).unwrap().set("Kids", vec![Object::Reference(root)]);
    refused(&mut cyclic, delete(), crate::named_dests::REFUSAL);

    let mut scripted = fixture();
    let dests = scripted.catalog().unwrap().get(b"Names").unwrap().as_dict().unwrap().get(b"Dests").unwrap().clone();
    scripted.catalog_mut().unwrap().set("Names", dictionary! { "Dests" => dests, "JavaScript" => dictionary! { "Names" => Vec::<Object>::new() } });
    refused(&mut scripted, delete(), "scripts");
    let mut session = EditSession::new(save_bytes(&mut scripted), 6);
    session.apply(PageEdit::Move { from: 0, to: 1 }).unwrap();
    assert!(session.export(None).is_ok(), "reordering does not need the names, so scripts do not block it");

    let mut open_action = fixture();
    let first = open_action.get_pages()[&1];
    open_action.catalog_mut().unwrap().set("OpenAction", fit(first));
    refused(&mut open_action, delete(), "open actions");

    let mut bad_labels = fixture();
    let labels = bad_labels.catalog().unwrap().get(b"PageLabels").unwrap().as_reference().unwrap();
    bad_labels.get_dictionary_mut(labels).unwrap().set("Nums", vec![Object::Integer(1), Object::Dictionary(dictionary! { "S" => "D" })]);
    refused(&mut bad_labels, PageEdit::Move { from: 0, to: 1 }, "physical page 0");

    let mut signed = fixture();
    signed.catalog_mut().unwrap().set("Perms", dictionary! { "DocMDP" => Object::Reference((9999, 0)) });
    refused(&mut signed, delete(), "Signed");
    let mut tagged = fixture();
    tagged.catalog_mut().unwrap().set("StructTreeRoot", dictionary! {});
    refused(&mut tagged, delete(), "tagged");
    let mut form = fixture();
    form.catalog_mut().unwrap().set("AcroForm", dictionary! {});
    refused(&mut form, PageEdit::Move { from: 0, to: 1 }, "forms");
}
