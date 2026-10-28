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
/// A page-number destination (`[3 /Fit]`): not conforming for a local target, but PDFium follows it as a page index.
fn fit_n(page: i64) -> Object { Object::Array(vec![Object::Integer(page), Object::Name(b"Fit".to_vec())]) }

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
/// strings and an external action; a two-leaf name tree with /Limits; labels i, ii, P-1, P-2, P-3, A. Bookmarks
/// E-K and the names tree-int-p0, Int-p4, Int-p2 and Real-p5 use page numbers instead of page references,
/// including a real (`[2.5 /Fit]`, truncated), no view mode (`[3]`) and a reference to a number, all of which PDFium follows.
fn fixture() -> lopdf::Document {
    let mut document = sample();
    let p: Vec<ObjectId> = document.get_pages().values().copied().collect();
    assert_eq!(p.len(), 6);
    let tree_p1 = document.add_object(fit(p[1]));
    let four = document.add_object(Object::Integer(4));
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
        node("E", Target::Dest(fit_n(3)), vec![node("E2", Target::Dest(string("tree-int-p0")), vec![])]),
        node("F", Target::Action(Object::Dictionary(dictionary! { "S" => "GoTo", "D" => vec![Object::Integer(1), "XYZ".into(), Object::Null, Object::Null, Object::Null] })), vec![
            node("F1", Target::Dest(Object::Name(b"Int-p4".to_vec())), vec![]),
        ]),
        node("G", Target::Dest(Object::Name(b"Int-p2".to_vec())), vec![]),
        node("H", Target::Dest(Object::Array(vec![Object::Real(2.5), Object::Name(b"Fit".to_vec())])), vec![]),
        node("I", Target::Dest(Object::Array(vec![Object::Integer(3)])), vec![]),
        node("J", Target::Dest(Object::Array(vec![Object::Reference(four), Object::Name(b"Fit".to_vec())])), vec![]),
        node("K", Target::Dest(Object::Name(b"Real-p5".to_vec())), vec![]),
    ]);
    let dests = document.add_object(dictionary! { "Named-p3" => fit(p[3]), "Named-p1" => dictionary! { "D" => fit(p[1]) }, "Int-p4" => fit_n(4), "Int-p2" => dictionary! { "D" => fit_n(2) }, "Real-p5" => vec![Object::Real(5.5), "XYZ".into(), Object::Null, Object::Null, Object::Null] });
    let leaf1 = document.add_object(dictionary! { "Limits" => vec![string("tree-int-p0"), string("tree-p2")], "Names" => vec![string("tree-int-p0"), fit_n(0), string("tree-p1"), Object::Reference(tree_p1), string("tree-p2"), fit(p[2])] });
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
    let h = original.objects.values().filter_map(|object| object.as_dict().ok()).find(|dict| dict.get(b"Title").ok().and_then(|title| lopdf::decode_text_string(title).ok()).as_deref() == Some("H")).unwrap();
    assert!(matches!(h.get(b"Dest").unwrap().as_array().unwrap()[0], Object::Real(_)), "the saved fixture keeps a real page number");
    let all = vec![row("A", 0, Some(0)), row("A1", 1, Some(1)), row("A1a", 2, Some(2)), row("A2", 1, Some(3)), row("B", 0, Some(1)), row("B1", 1, Some(4)), row("B2", 1, Some(5)), row("C", 0, None), row("D", 0, Some(5)), row("D1", 1, Some(3)), row("E", 0, Some(3)), row("E2", 1, Some(0)), row("F", 0, Some(1)), row("F1", 1, Some(4)), row("G", 0, Some(2)), row("H", 0, Some(2)), row("I", 0, Some(3)), row("J", 0, Some(4)), row("K", 0, Some(5))];
    assert_eq!(lopdf_rows(&original), all, "the reader resolves direct, GoTo, /Dests and name-tree destinations");
    assert_eq!(pdfium_rows(&service, &info), all, "PDFium follows page-number destinations as page indexes");
    assert_eq!(pdfium_labels(&service, &info), ["i", "ii", "P-1", "P-2", "P-3", "A"]);

    let deleted = direct(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1, 4] }, reply)).unwrap();
    let (copy, rows, labels) = saved(&service, &deleted, folder.path(), "deleted.pdf", None);
    assert_eq!(rows, vec![row("A", 0, Some(0)), row("A1a", 1, Some(1)), row("A2", 1, Some(2)), row("B2", 0, Some(3)), row("C", 0, None), row("D", 0, Some(3)), row("D1", 1, Some(2)), row("E", 0, Some(2)), row("E2", 1, Some(0)), row("G", 0, Some(1)), row("H", 0, Some(1)), row("I", 0, Some(2)), row("K", 0, Some(3))],
        "bookmarks to removed pages are dropped and their children promoted; the rest keep their pages");
    assert_eq!(verify(&copy).iter().map(|(title, _, open)| (title.as_str(), *open)).filter(|(title, _)| *title == "D").collect::<Vec<_>>(), [("D", false)], "a closed bookmark stays closed");
    assert_eq!(labels, ["i", "P-1", "P-2", "A"], "each kept page keeps its original label");
    assert_eq!(names(&copy), vec![("Int-p2".into(), Some(1)), ("Named-p3".into(), Some(2)), ("Real-p5".into(), Some(3)), ("tree-int-p0".into(), Some(0)), ("tree-p2".into(), Some(1)), ("tree-p5".into(), Some(3))], "named destinations to removed pages are dropped");
    assert_eq!(copy.get_pages().len(), 4);
    assert!(!crate::named_dests::page_number_destination_left(&copy, 6), "page numbers are turned into page references");
    for title in ["A1", "B", "B1", "F", "F1", "J"] { assert!(!has_title(&copy, title), "the dropped bookmark {title} is pruned from the copy"); }
    assert_eq!(copy.objects.values().filter(|object| object.as_dict().is_ok_and(|dict| dict.get(b"Type").and_then(Object::as_name).is_ok_and(|kind| kind == b"Page"))).count(), 4, "removed pages are pruned");
    assert!(!copy.objects.values().any(|object| object.as_array().is_ok_and(|array| matches!(array.first(), Some(Object::Null)))), "no destination is left pointing at null");

    let moved = direct(&service, |reply| Request::Edit(deleted.id, PageEdit::Undo, reply)).unwrap();
    let moved = { assert!(!moved.dirty); direct(&service, |reply| Request::Edit(moved.id, PageEdit::Move { from: 0, to: 5 }, reply)).unwrap() };
    let (copy, rows, labels) = saved(&service, &moved, folder.path(), "moved.pdf", None);
    assert_eq!(rows, vec![row("A", 0, Some(5)), row("A1", 1, Some(0)), row("A1a", 2, Some(1)), row("A2", 1, Some(2)), row("B", 0, Some(0)), row("B1", 1, Some(3)), row("B2", 1, Some(4)), row("C", 0, None), row("D", 0, Some(4)), row("D1", 1, Some(2)),
        row("E", 0, Some(2)), row("E2", 1, Some(5)), row("F", 0, Some(0)), row("F1", 1, Some(3)), row("G", 0, Some(1)), row("H", 0, Some(1)), row("I", 0, Some(2)), row("J", 0, Some(3)), row("K", 0, Some(4))], "page-number bookmarks follow their pages on a move");
    assert_eq!(names(&copy), vec![("Int-p2".into(), Some(1)), ("Int-p4".into(), Some(3)), ("Named-p1".into(), Some(0)), ("Named-p3".into(), Some(2)), ("Real-p5".into(), Some(4)), ("tree-int-p0".into(), Some(5)), ("tree-p1".into(), Some(0)), ("tree-p2".into(), Some(1)), ("tree-p5".into(), Some(4))]);
    assert!(!crate::named_dests::page_number_destination_left(&copy, 6));
    assert_eq!(labels, ["ii", "P-1", "P-2", "P-3", "A", "i"], "a moved page keeps its label");

    let fresh = open(&service, &source);
    let (copy, rows, labels) = saved(&service, &fresh, folder.path(), "extracted.pdf", Some(vec![2, 3]));
    assert_eq!(rows, vec![row("A1a", 0, Some(0)), row("A2", 0, Some(1)), row("C", 0, None), row("D1", 0, Some(1)), row("E", 0, Some(1)), row("G", 0, Some(0)), row("H", 0, Some(0)), row("I", 0, Some(1))]);
    assert_eq!(labels, ["P-1", "P-2"]);
    assert_eq!(names(&copy), vec![("Int-p2".into(), Some(0)), ("Named-p3".into(), Some(1)), ("tree-p2".into(), Some(0))]);
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
    assert_eq!(lopdf_rows(&copy), vec![row("A", 0, Some(0)), row("A1a", 1, Some(1)), row("Renamed", 1, Some(2)), row("B2", 0, Some(3)), row("C", 0, None), row("D", 0, Some(3)), row("D1", 1, Some(2)), row("E", 0, Some(2)), row("E2", 1, Some(0)), row("G", 0, Some(1)), row("H", 0, Some(1)), row("I", 0, Some(2)), row("K", 0, Some(3))]);
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
    let (a1, d) = (title(&shared, "A1"), title(&shared, "K"));
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

    let mut duplicate = fixture();
    let root = duplicate.catalog().unwrap().get(b"Names").unwrap().as_dict().unwrap().get(b"Dests").unwrap().as_reference().unwrap();
    let leaf2 = duplicate.get_dictionary(root).unwrap().get(b"Kids").unwrap().as_array().unwrap()[1].as_reference().unwrap();
    let page = duplicate.get_pages()[&1];
    duplicate.get_dictionary_mut(leaf2).unwrap().set("Names", vec![string("tree-p2"), fit(page), string("tree-p5"), fit(page)]);
    refused(&mut duplicate, delete(), crate::named_dests::REFUSAL);

    let mut numbered = fixture();
    numbered.catalog_mut().unwrap().set("OpenAction", fit_n(0));
    let bytes = save_bytes(&mut numbered);
    let mut session = EditSession::new(bytes.clone(), 6);
    let error = session.apply(PageEdit::Move { from: 0, to: 1 }).unwrap_err();
    assert!(error.contains("by its number"), "a page-number destination this copy cannot update refuses the move: {error}");
    assert_eq!(session.revision, 0); assert_eq!(session.source, bytes);
    let mut plain = sample();
    plain.catalog_mut().unwrap().set("OpenAction", Object::Array(vec![Object::Real(1.5)]));
    let mut session = EditSession::new(save_bytes(&mut plain), 6);
    assert!(session.apply(PageEdit::Move { from: 0, to: 1 }).unwrap_err().contains("by its number"), "without outlines, names or labels the cached source scan still refuses");
    numbered.catalog_mut().unwrap().set("OpenAction", dictionary! { "S" => "GoToR", "F" => string("other.pdf"), "D" => fit_n(0) });
    let mut session = EditSession::new(save_bytes(&mut numbered), 6);
    session.apply(PageEdit::Move { from: 0, to: 1 }).unwrap();
    assert!(session.export(None).is_ok(), "a page number in a remote GoToR names a page of another file and is left alone");

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

/// A generated 2,000-page file of about 190 MiB (each page a ~96 KiB incompressible-text content stream).
fn large(outline_and_labels: bool) -> Vec<u8> {
    let mut document = lopdf::Document::with_version("1.7");
    let pages = document.new_object_id();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut kids = Vec::new();
    for _ in 0..2000 {
        let mut body = b"q Q\n%".to_vec();
        for _ in 0..96 * 1024 { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; body.push(b'a' + (seed % 26) as u8); }
        let content = document.add_object(lopdf::Stream::new(dictionary! {}, body));
        kids.push(Object::Reference(document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Contents" => content })));
    }
    document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => 2000, "Kids" => kids }));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    if outline_and_labels {
        add_outline(&mut document, (0..10).map(|_| node("Chapter", Target::Dest(fit_n(0)), vec![])).collect());
        let labels = document.add_object(dictionary! { "Nums" => vec![0.into(), dictionary! { "S" => "r" }.into(), 10.into(), dictionary! { "S" => "D" }.into()] });
        document.catalog_mut().unwrap().set("PageLabels", labels);
    }
    save_bytes(&mut document)
}

#[test]
#[ignore = "timing; run with --release --ignored --nocapture"]
fn timing_move_and_delete_on_a_large_file() {
    for outline_and_labels in [false, true] {
        let bytes = large(outline_and_labels);
        let mut session = EditSession::new(bytes.clone(), 2000);
        let mut times = Vec::new();
        for round in 0..3 {
            let start = std::time::Instant::now();
            session.apply(PageEdit::Move { from: round, to: 1999 }).unwrap();
            times.push(start.elapsed().as_millis());
        }
        let start = std::time::Instant::now();
        session.apply(PageEdit::Delete { pages: vec![5] }).unwrap();
        let delete = start.elapsed().as_millis();
        eprintln!("TIMING outline_and_labels={outline_and_labels} size={} MiB moves={times:?} ms delete={delete} ms", bytes.len() / (1024 * 1024));
    }
}
