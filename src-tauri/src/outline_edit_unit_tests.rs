use super::*;
use lopdf::StringFormat;

pub(crate) enum Node { Item(&'static str, Option<usize>, Vec<Node>), Closed(&'static str, Option<usize>, Vec<Node>) }
use Node::{Closed, Item};

pub(crate) fn sample() -> Document { Document::load(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }

/// Builds an outline the way a PDF producer would, with correct `/Parent /First /Last /Prev /Next /Count`.
pub(crate) fn with_outline(document: &mut Document, nodes: &[Node]) {
    let pages: Vec<ObjectId> = document.get_pages().values().copied().collect();
    fn level(document: &mut Document, pages: &[ObjectId], parent: ObjectId, nodes: &[Node]) -> (Option<ObjectId>, Option<ObjectId>, i64) {
        let ids: Vec<ObjectId> = nodes.iter().map(|_| document.new_object_id()).collect();
        let mut visible = 0i64;
        for (index, node) in nodes.iter().enumerate() {
            let (title, page, kids, closed) = match node { Item(t, p, k) => (*t, *p, k, false), Closed(t, p, k) => (*t, *p, k, true) };
            let mut dict = dictionary! { "Title" => Object::string_literal(title), "Parent" => parent };
            if let Some(page) = page { dict.set("Dest", vec![Object::Reference(pages[page]), Object::Name(b"Fit".to_vec())]); }
            if index > 0 { dict.set("Prev", ids[index - 1]); }
            if index + 1 < ids.len() { dict.set("Next", ids[index + 1]); }
            let (first, last, count) = level(document, pages, ids[index], kids);
            if let (Some(first), Some(last)) = (first, last) {
                dict.set("First", first); dict.set("Last", last); dict.set("Count", if closed { -count } else { count });
                visible += 1 + if closed { 0 } else { count };
            } else { visible += 1; }
            document.objects.insert(ids[index], Object::Dictionary(dict));
        }
        (ids.first().copied(), ids.last().copied(), visible)
    }
    let root = document.new_object_id();
    let (first, last, count) = level(document, &pages, root, nodes);
    document.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => first.unwrap(), "Last" => last.unwrap(), "Count" => count }));
    document.catalog_mut().unwrap().set("Outlines", root);
}

pub(crate) fn plan(count: usize) -> Vec<PageSpec> { (0..count).map(|source| PageSpec { source, turns: 0, crop: None, notes: Vec::new() }).collect() }

fn titles(edits: &OutlineEdits) -> Vec<(String, usize)> { edits.current.iter().map(|entry| (entry.title.clone(), entry.depth)).collect() }
fn outline(entries: &[(&str, usize)]) -> Vec<(String, usize)> { entries.iter().map(|(title, depth)| ((*title).to_owned(), *depth)).collect() }
fn title_of(document: &Document, id: ObjectId) -> Option<String> { document.get_dictionary(id).ok()?.get(b"Title").ok().and_then(|t| lopdf::decode_text_string(t).ok()) }
fn find(document: &Document, name: &str) -> ObjectId { document.objects.keys().copied().find(|id| title_of(document, *id).as_deref() == Some(name)).unwrap() }

/// Walks a written outline with an independent checker and returns `(title, depth, open)` in order.
/// Fails the test on any broken link or wrong `/Count`.
pub(crate) fn verify(document: &Document) -> Vec<(String, usize, bool)> {
    let root_id = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
    let root = document.get_dictionary(root_id).unwrap();
    let mut out = Vec::new();
    fn walk(document: &Document, parent: ObjectId, first: Option<ObjectId>, last: Option<ObjectId>, depth: usize, out: &mut Vec<(String, usize, bool)>) -> i64 {
        let mut visible = 0i64; let mut previous: Option<ObjectId> = None; let mut cursor = first;
        while let Some(id) = cursor {
            let dict = document.get_dictionary(id).unwrap();
            assert_eq!(dict.get(b"Parent").unwrap().as_reference().unwrap(), parent, "/Parent of {id:?}");
            assert_eq!(dict.get(b"Prev").ok().map(|o| o.as_reference().unwrap()), previous, "/Prev of {id:?}");
            let title = lopdf::decode_text_string(dict.get(b"Title").unwrap()).unwrap();
            let index = out.len(); out.push((title, depth, false));
            let child_first = dict.get(b"First").ok().map(|o| o.as_reference().unwrap());
            let child_last = dict.get(b"Last").ok().map(|o| o.as_reference().unwrap());
            assert_eq!(child_first.is_some(), child_last.is_some());
            let descendants = walk(document, id, child_first, child_last, depth + 1, out);
            match dict.get(b"Count") {
                Ok(count) => {
                    let count = count.as_i64().unwrap();
                    assert!(child_first.is_some(), "/Count on a leaf");
                    assert_eq!(count.abs(), descendants, "|/Count| of {id:?}");
                    out[index].2 = count > 0; visible += 1 + count.max(0);
                }
                Err(_) => { assert!(child_first.is_none(), "children without /Count"); visible += 1; }
            }
            previous = Some(id); cursor = dict.get(b"Next").ok().map(|o| o.as_reference().unwrap());
        }
        assert_eq!(previous, last, "/Last of {parent:?}");
        visible
    }
    let total = walk(document, root_id, root.get(b"First").ok().map(|o| o.as_reference().unwrap()), root.get(b"Last").ok().map(|o| o.as_reference().unwrap()), 0, &mut out);
    assert_eq!(root.get(b"Count").unwrap().as_i64().unwrap(), total, "/Count of the outline root");
    out
}

fn document_pages(source: usize) -> Option<ObjectId> { sample().get_pages().values().nth(source).copied() }
fn loaded(nodes: &[Node]) -> (Document, OutlineEdits) {
    let mut document = sample(); with_outline(&mut document, nodes);
    let mut edits = OutlineEdits::default(); edits.load(read(&document)); (document, edits)
}
fn tree() -> Vec<Node> {
    vec![
        Item("A", Some(0), vec![Item("A1", Some(1), vec![]), Item("A2", Some(2), vec![Item("A2x", Some(2), vec![])])]),
        Closed("B", Some(3), vec![Item("B1", Some(4), vec![])]),
        Item("C", Some(5), vec![]),
    ]
}
fn op(edits: &mut OutlineEdits, operation: BookmarkOp) -> Result<(), String> { let version = edits.version; edits.apply(version, operation, &plan(6)) }
fn uid(edits: &OutlineEdits, title: &str) -> u32 { edits.current.iter().find(|entry| entry.title == title).unwrap().uid }

#[test]
fn reads_preorder_depth_open_state_and_pages() {
    let (_, edits) = loaded(&tree());
    assert_eq!(titles(&edits), outline(&[("A", 0), ("A1", 1), ("A2", 1), ("A2x", 2), ("B", 0), ("B1", 1), ("C", 0)]));
    let open: Vec<_> = edits.current.iter().map(|entry| entry.open).collect();
    assert_eq!(open, [true, false, true, false, false, false, false], "A and A2 are open parents, B is closed");
    let pages: Vec<_> = edits.current.iter().map(|entry| entry.source_page).collect();
    assert_eq!(pages, [Some(0), Some(1), Some(2), Some(2), Some(3), Some(4), Some(5)]);
    assert!(!edits.changed());
}

#[test]
fn written_tree_has_correct_links_counts_and_signs_after_every_operation() {
    let (_, mut edits) = loaded(&tree());
    let (a2, b, c, a1) = (uid(&edits, "A2"), uid(&edits, "B"), uid(&edits, "C"), uid(&edits, "A1"));
    let steps: Vec<(BookmarkOp, Vec<(&str, usize)>)> = vec![
        (BookmarkOp::MoveUp { uid: a2 }, vec![("A", 0), ("A2", 1), ("A2x", 2), ("A1", 1), ("B", 0), ("B1", 1), ("C", 0)]),
        (BookmarkOp::MoveDown { uid: a2 }, vec![("A", 0), ("A1", 1), ("A2", 1), ("A2x", 2), ("B", 0), ("B1", 1), ("C", 0)]),
        (BookmarkOp::Indent { uid: c }, vec![("A", 0), ("A1", 1), ("A2", 1), ("A2x", 2), ("B", 0), ("B1", 1), ("C", 1)]),
        (BookmarkOp::Outdent { uid: a1 }, vec![("A", 0), ("A2", 1), ("A2x", 2), ("A1", 0), ("B", 0), ("B1", 1), ("C", 1)]),
        (BookmarkOp::Delete { uid: b }, vec![("A", 0), ("A2", 1), ("A2x", 2), ("A1", 0)]),
    ];
    for (operation, expected) in steps {
        op(&mut edits, operation).unwrap();
        assert_eq!(titles(&edits), outline(&expected));
        let mut document = sample(); with_outline(&mut document, &tree());
        write(&mut document, &edits.current, &document_pages).unwrap();
        let written = verify(&document);
        assert_eq!(written.iter().map(|(title, depth, _)| (title.clone(), *depth)).collect::<Vec<_>>(), outline(&expected));
    }
}

#[test]
fn open_and_closed_state_is_written_as_count_sign() {
    let (mut document, mut edits) = loaded(&tree());
    let (a, b) = (uid(&edits, "A"), uid(&edits, "B"));
    op(&mut edits, BookmarkOp::SetOpen { uid: a, open: false }).unwrap();
    op(&mut edits, BookmarkOp::SetOpen { uid: b, open: true }).unwrap();
    write(&mut document, &edits.current, &document_pages).unwrap();
    let written = verify(&document);
    assert_eq!(written.iter().filter(|(title, _, open)| matches!(title.as_str(), "A" | "B") && *open).map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(), ["B"]);
    let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
    // A is closed (its three descendants are hidden), B is open with one child, C is a leaf: A, B, B1, C.
    assert_eq!(document.get_dictionary(root).unwrap().get(b"Count").unwrap().as_i64().unwrap(), 4);
    let a_dict = document.get_dictionary(find(&document, "A")).unwrap();
    assert_eq!(a_dict.get(b"Count").unwrap().as_i64().unwrap(), -3, "a closed item counts the descendants it would show if opened");
}

#[test]
fn untouched_bookmarks_keep_their_destinations_actions_and_flags() {
    let mut document = sample(); with_outline(&mut document, &tree());
    let a2 = find(&document, "A2");
    let action = dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") };
    {
        let dict = document.get_dictionary_mut(a2).unwrap();
        dict.remove(b"Dest"); dict.set("A", action.clone());
        dict.set("C", vec![Object::Real(1.0), Object::Real(0.0), Object::Real(0.0)]); dict.set("F", 3); dict.set("SE", Object::Reference((9999, 0)));
    }
    let fit = document.get_dictionary(find(&document, "B1")).unwrap().get(b"Dest").unwrap().clone();
    let mut edits = OutlineEdits::default(); edits.load(read(&document));
    let a1 = uid(&edits, "A1"); op(&mut edits, BookmarkOp::Rename { uid: a1, title: "Renamed".into() }).unwrap();
    let c = uid(&edits, "C"); op(&mut edits, BookmarkOp::MoveUp { uid: c }).unwrap();
    write(&mut document, &edits.current, &document_pages).unwrap();
    let dict = document.get_dictionary(a2).unwrap();
    assert_eq!(dict.get(b"A").unwrap(), &Object::Dictionary(action));
    assert!(!dict.has(b"Dest"));
    assert_eq!(dict.get(b"F").unwrap().as_i64().unwrap(), 3);
    assert!(dict.has(b"C") && dict.has(b"SE"));
    assert_eq!(title_of(&document, a2).unwrap(), "A2", "an unrenamed title is not rewritten");
    assert_eq!(document.get_dictionary(find(&document, "B1")).unwrap().get(b"Dest").unwrap(), &fit);
    verify(&document);
}

#[test]
fn add_rename_and_delete_with_children() {
    let (mut document, mut edits) = loaded(&tree());
    let a2 = uid(&edits, "A2");
    op(&mut edits, BookmarkOp::Add { page: 3, title: "  New  ".into(), after: Some(a2) }).unwrap();
    assert_eq!(titles(&edits), outline(&[("A", 0), ("A1", 1), ("A2", 1), ("A2x", 2), ("New", 1), ("B", 0), ("B1", 1), ("C", 0)]), "added after the selected bookmark's subtree, as its sibling");
    op(&mut edits, BookmarkOp::Add { page: 5, title: "Last".into(), after: None }).unwrap();
    assert_eq!(edits.current.last().unwrap().depth, 0);
    let new = uid(&edits, "New"); op(&mut edits, BookmarkOp::Rename { uid: new, title: "Zażółć".into() }).unwrap();
    let a = uid(&edits, "A"); op(&mut edits, BookmarkOp::Delete { uid: a }).unwrap();
    assert_eq!(titles(&edits), outline(&[("B", 0), ("B1", 1), ("C", 0), ("Last", 0)]), "deleting a bookmark deletes its children");
    write(&mut document, &edits.current, &document_pages).unwrap();
    verify(&document);
    let again = read(&document).unwrap();
    assert_eq!(again.iter().map(|e| (e.title.as_str(), e.depth)).collect::<Vec<_>>(), [("B", 0), ("B1", 1), ("C", 0), ("Last", 0)]);
    assert_eq!(again[3].source_page, Some(5));
    let mut unicode = sample();
    let entry = Entry { uid: 99, origin: None, title: "Zażółć".into(), renamed: false, depth: 0, open: true, source_page: Some(0) };
    write(&mut unicode, &[entry], &document_pages).unwrap();
    let read_back = read(&unicode).unwrap();
    assert_eq!(read_back[0].title, "Zażółć", "non-ASCII titles survive");
    let text = unicode.get_dictionary(read_back[0].origin.unwrap()).unwrap().get(b"Title").unwrap();
    assert!(matches!(text, Object::String(bytes, StringFormat::Literal) if bytes.starts_with(&[0xfe, 0xff])), "stored as UTF-16BE with a byte order mark");
}

#[test]
fn indent_outdent_and_move_edge_cases_refuse_without_changing_anything() {
    let (_, mut edits) = loaded(&tree());
    let (a, a1, c) = (uid(&edits, "A"), uid(&edits, "A1"), uid(&edits, "C"));
    let before = edits.current.clone(); let version = edits.version;
    for operation in [
        BookmarkOp::MoveUp { uid: a }, BookmarkOp::MoveDown { uid: c }, BookmarkOp::Indent { uid: a }, BookmarkOp::Indent { uid: a1 }, BookmarkOp::Outdent { uid: a },
        BookmarkOp::Delete { uid: 999 }, BookmarkOp::Rename { uid: a, title: "   ".into() }, BookmarkOp::Rename { uid: a, title: "bad\u{7}".into() },
        BookmarkOp::Add { page: 6, title: "x".into(), after: None }, BookmarkOp::Undo, BookmarkOp::Redo,
    ] { assert!(op(&mut edits, operation.clone()).is_err(), "{operation:?}"); }
    assert_eq!(edits.current, before); assert_eq!(edits.version, version); assert!(!edits.changed());
    op(&mut edits, BookmarkOp::Rename { uid: a, title: "A".into() }).unwrap();
    assert_eq!(edits.version, version, "renaming to the same title is a no-op");
}

#[test]
fn outdent_places_the_item_after_its_parent_and_following_siblings_stay_with_the_parent() {
    let (_, mut edits) = loaded(&tree());
    let a1 = uid(&edits, "A1"); op(&mut edits, BookmarkOp::Outdent { uid: a1 }).unwrap();
    assert_eq!(titles(&edits), outline(&[("A", 0), ("A2", 1), ("A2x", 2), ("A1", 0), ("B", 0), ("B1", 1), ("C", 0)]));
    let a2x = uid(&edits, "A2x"); op(&mut edits, BookmarkOp::Outdent { uid: a2x }).unwrap();
    assert_eq!(titles(&edits), outline(&[("A", 0), ("A2", 1), ("A2x", 1), ("A1", 0), ("B", 0), ("B1", 1), ("C", 0)]));
}

#[test]
fn indent_opens_the_new_parent_and_respects_the_depth_limit() {
    let (_, mut edits) = loaded(&[Closed("P", Some(0), vec![Item("kid", Some(1), vec![])]), Item("Q", Some(2), vec![])]);
    let q = uid(&edits, "Q"); op(&mut edits, BookmarkOp::Indent { uid: q }).unwrap();
    assert!(edits.current[0].open, "indenting into a closed bookmark opens it so the result is visible");
    let mut chain = vec![Item("leaf", None, vec![])]; for _ in 0..MAX_DEPTH - 1 { chain = vec![Item("n", None, chain)]; }
    chain.insert(0, Item("R1", None, vec![]));
    let (_, mut edits) = loaded(&chain);
    assert_eq!(edits.current.iter().map(|entry| entry.depth).max(), Some(MAX_DEPTH - 1));
    let chain_root = edits.current[1].uid; let before = edits.current.clone();
    assert!(op(&mut edits, BookmarkOp::Indent { uid: chain_root }).unwrap_err().contains("at most"));
    assert_eq!(edits.current, before);
}

#[test]
fn undo_redo_and_stale_versions() {
    let (_, mut edits) = loaded(&tree());
    let c = uid(&edits, "C"); let stale = edits.version;
    op(&mut edits, BookmarkOp::Delete { uid: c }).unwrap();
    assert!(edits.changed());
    assert!(edits.apply(stale, BookmarkOp::Undo, &plan(6)).unwrap_err().contains("changed"));
    op(&mut edits, BookmarkOp::Undo).unwrap(); assert!(!edits.changed(), "undoing back to the source is not an unsaved change");
    op(&mut edits, BookmarkOp::Redo).unwrap(); assert!(edits.changed());
    op(&mut edits, BookmarkOp::Undo).unwrap();
    op(&mut edits, BookmarkOp::Rename { uid: c, title: "Z".into() }).unwrap();
    assert!(op(&mut edits, BookmarkOp::Redo).is_err(), "a new edit clears redo");
    let view = edits.view(&plan(6));
    assert!(view.editable && view.changed && view.can_undo && !view.can_redo);
    for index in 0..40 { op(&mut edits, BookmarkOp::Rename { uid: c, title: format!("Z{index}") }).unwrap(); }
    assert!(edits.undo.len() <= MAX_UNDO);
}

#[test]
fn view_maps_source_pages_to_the_current_page_order() {
    let (_, edits) = loaded(&tree());
    let mut order = plan(6); order.remove(1); order.swap(0, 3);
    let positions: Vec<_> = edits.view(&order).entries.iter().map(|entry| entry.page).collect();
    // The page order is now sources [4, 2, 3, 0, 5]; source page 1 was deleted.
    assert_eq!(positions, [Some(3), None, Some(1), Some(1), Some(2), Some(0), Some(4)]);
}

#[test]
fn a_new_bookmark_on_a_page_that_is_not_in_the_output_is_dropped_and_its_children_move_up() {
    let mut document = sample();
    let new = |uid: u32, title: &str, depth: usize, page: usize| Entry { uid, origin: None, title: title.into(), renamed: false, depth, open: true, source_page: Some(page) };
    let entries = vec![new(0, "Keep", 0, 0), new(1, "Gone", 1, 4), new(2, "Child", 2, 1), new(3, "Sibling", 1, 2)];
    let pages: Vec<_> = document.get_pages().values().copied().collect();
    write(&mut document, &entries, &|source| (source != 4).then(|| pages[source])).unwrap();
    let written = verify(&document);
    assert_eq!(written.iter().map(|(t, d, _)| (t.as_str(), *d)).collect::<Vec<_>>(), [("Keep", 0), ("Child", 1), ("Sibling", 1)]);
    let mut empty = sample();
    write(&mut empty, &[new(9, "Only", 0, 4)], &|source| (source != 4).then(|| pages[source])).unwrap();
    assert!(!empty.catalog().unwrap().has(b"Outlines"), "an outline left with nothing to show is removed");
}

#[test]
fn deleting_every_bookmark_removes_the_outline_entry() {
    let (mut document, mut edits) = loaded(&tree());
    for title in ["A", "B", "C"] { let uid = uid(&edits, title); op(&mut edits, BookmarkOp::Delete { uid }).unwrap(); }
    assert!(edits.current.is_empty() && edits.changed());
    write(&mut document, &edits.current, &document_pages).unwrap();
    assert!(!document.catalog().unwrap().has(b"Outlines"));
}

#[test]
fn a_wrong_count_is_repaired_and_a_direct_root_dictionary_is_accepted() {
    let mut document = sample(); with_outline(&mut document, &tree());
    let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
    for object in document.objects.values_mut() { if let Ok(dict) = object.as_dict_mut() { if dict.has(b"Count") { dict.set("Count", 77); } } }
    let mut edits = OutlineEdits::default(); edits.load(read(&document));
    assert_eq!(edits.current.len(), 7);
    let a1 = uid(&edits, "A1"); op(&mut edits, BookmarkOp::Delete { uid: a1 }).unwrap();
    write(&mut document, &edits.current, &document_pages).unwrap();
    verify(&document);
    let direct = document.get_dictionary(root).unwrap().clone();
    document.catalog_mut().unwrap().set("Outlines", Object::Dictionary(direct));
    let mut edits = OutlineEdits::default(); edits.load(read(&document));
    let c = uid(&edits, "C"); op(&mut edits, BookmarkOp::Delete { uid: c }).unwrap();
    write(&mut document, &edits.current, &document_pages).unwrap();
    verify(&document);
}

fn relink(document: &mut Document, title: &str, key: &str, target: &str) {
    let (from, to) = (find(document, title), find(document, target));
    document.get_dictionary_mut(from).unwrap().set(key, Object::Reference(to));
}

#[test]
fn malformed_outlines_are_refused_with_bounded_walks() {
    let refused = |mutate: &dyn Fn(&mut Document), expect: &str| {
        let mut document = sample(); with_outline(&mut document, &tree()); mutate(&mut document);
        let error = read(&document).unwrap_err(); assert!(error.contains(expect), "{error}");
    };
    refused(&|d| relink(d, "C", "Next", "A"), "cycle");
    refused(&|d| relink(d, "A2x", "First", "A"), "cycle");
    refused(&|d| relink(d, "A1", "Next", "A1"), "cycle");
    refused(&|d| relink(d, "B", "Next", "A2x"), "cycle");
    refused(&|d| { let id = find(d, "C"); d.objects.insert(id, Object::Integer(1)); }, "not a dictionary");
    refused(&|d| { let id = d.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap(); d.get_dictionary_mut(id).unwrap().set("First", Object::Integer(3)); }, "unsupported");
    refused(&|d| d.catalog_mut().unwrap().set("Outlines", Object::Integer(3)), "unsupported");
    let mut wide = sample(); let nodes: Vec<Node> = (0..1001).map(|_| Item("x", Some(0), vec![])).collect(); with_outline(&mut wide, &nodes);
    assert!(read(&wide).unwrap_err().contains("more than 1000"));
    let mut too_deep = vec![Item("leaf", None, vec![])]; for _ in 0..MAX_DEPTH { too_deep = vec![Item("n", None, too_deep)]; }
    let mut deep = sample(); with_outline(&mut deep, &too_deep); assert!(read(&deep).unwrap_err().contains("nested"));
    let mut allowed = vec![Item("leaf", None, vec![])]; for _ in 0..MAX_DEPTH - 1 { allowed = vec![Item("n", None, allowed)]; }
    let mut fits = sample(); with_outline(&mut fits, &allowed); assert_eq!(read(&fits).unwrap().len(), MAX_DEPTH);
    let mut huge = sample(); let title: &'static str = Box::leak("t".repeat(600_000).into_boxed_str());
    with_outline(&mut huge, &[Item(title, None, vec![]), Item(title, None, vec![])]);
    assert!(read(&huge).unwrap_err().contains("too much text"));
}

#[test]
fn a_refused_outline_makes_editing_unavailable_with_the_reason() {
    let mut document = sample(); with_outline(&mut document, &tree()); relink(&mut document, "C", "Next", "A");
    let mut edits = OutlineEdits::default(); edits.load(read(&document));
    let view = edits.view(&plan(6));
    assert!(!view.editable && view.entries.is_empty() && view.reason.unwrap().contains("cycle"));
    assert!(op_unloaded(&mut edits).contains("cycle"));
}
fn op_unloaded(edits: &mut OutlineEdits) -> String { edits.apply(0, BookmarkOp::Delete { uid: 0 }, &plan(6)).unwrap_err() }

#[test]
fn the_entry_limit_applies_to_additions() {
    let mut document = sample(); let nodes: Vec<Node> = (0..MAX_ENTRIES).map(|_| Item("x", Some(0), vec![])).collect(); with_outline(&mut document, &nodes);
    let mut edits = OutlineEdits::default(); edits.load(read(&document)); assert_eq!(edits.current.len(), MAX_ENTRIES);
    assert!(op(&mut edits, BookmarkOp::Add { page: 0, title: "one more".into(), after: None }).unwrap_err().contains("at most 1000"));
}

#[test]
fn a_document_without_bookmarks_gains_a_valid_outline() {
    let mut document = sample(); assert!(!document.catalog().unwrap().has(b"Outlines"));
    let mut edits = OutlineEdits::default(); edits.load(read(&document)); assert!(edits.current.is_empty());
    op(&mut edits, BookmarkOp::Add { page: 2, title: "Chapter".into(), after: None }).unwrap();
    let chapter = uid(&edits, "Chapter");
    op(&mut edits, BookmarkOp::Add { page: 3, title: "Section".into(), after: Some(chapter) }).unwrap();
    let section = uid(&edits, "Section"); op(&mut edits, BookmarkOp::Indent { uid: section }).unwrap();
    write(&mut document, edits.pending().unwrap(), &document_pages).unwrap();
    assert_eq!(verify(&document), [("Chapter".to_owned(), 0, true), ("Section".to_owned(), 1, false)]);
    let pages = read(&document).unwrap().iter().map(|entry| entry.source_page).collect::<Vec<_>>(); assert_eq!(pages, [Some(2), Some(3)]);
}

fn page_tree_bytes(document: &Document) -> Vec<(ObjectId, Vec<u8>)> {
    let mut ids: Vec<ObjectId> = vec![document.trailer.get(b"Root").unwrap().as_reference().unwrap()];
    ids.extend(document.get_pages().values().copied());
    ids.push(document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap());
    ids.sort(); ids.dedup();
    ids.into_iter().map(|id| (id, format!("{:?}", document.get_object(id).unwrap()).into_bytes())).collect()
}

#[test]
fn hostile_outlines_aimed_at_the_page_tree_are_refused_and_leave_it_untouched() {
    let hostile = |aim: &dyn Fn(&Document) -> ObjectId, via_root: bool, expect: &str| {
        let mut document = sample(); with_outline(&mut document, &tree());
        let target = aim(&document);
        let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
        if via_root { document.get_dictionary_mut(root).unwrap().set("First", Object::Reference(target)); } else { let a = find(&document, "A"); document.get_dictionary_mut(a).unwrap().set("First", Object::Reference(target)); }
        let before = page_tree_bytes(&document);
        let mut edits = OutlineEdits::default(); edits.load(read(&document));
        let view = edits.view(&plan(6));
        assert!(!view.editable, "{expect}");
        assert!(view.reason.unwrap().contains(expect));
        assert!(edits.apply(edits.version, BookmarkOp::Add { page: 0, title: "x".into(), after: None }, &plan(6)).is_err());
        assert!(edits.pending().is_none());
        assert_eq!(page_tree_bytes(&document), before);
    };
    hostile(&|d| d.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap(), true, "page tree or catalog");
    hostile(&|d| d.get_pages().values().next().copied().unwrap(), true, "page tree or catalog");
    hostile(&|d| d.trailer.get(b"Root").unwrap().as_reference().unwrap(), true, "page tree or catalog");
    hostile(&|d| d.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap(), false, "page tree or catalog");
    hostile(&|d| d.get_pages().values().nth(1).copied().unwrap(), false, "page tree or catalog");
    let mut document = sample();
    let pages = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
    document.catalog_mut().unwrap().set("Outlines", Object::Reference(pages));
    assert!(read(&document).unwrap_err().contains("page tree or catalog"));
}

#[test]
fn outline_items_with_a_wrong_parent_or_type_are_refused() {
    let refused = |mutate: &dyn Fn(&mut Document), expect: &str| {
        let mut document = sample(); with_outline(&mut document, &tree()); mutate(&mut document);
        let error = read(&document).unwrap_err(); assert!(error.contains(expect), "{error}");
    };
    refused(&|d| { let (a, c) = (find(d, "A"), find(d, "C")); d.get_dictionary_mut(c).unwrap().set("Parent", Object::Reference(a)); }, "parent link");
    refused(&|d| { let (a1, root) = (find(d, "A1"), d.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap()); d.get_dictionary_mut(a1).unwrap().set("Parent", Object::Reference(root)); }, "parent link");
    refused(&|d| { let c = find(d, "C"); d.get_dictionary_mut(c).unwrap().set("Parent", Object::Integer(1)); }, "parent link");
    refused(&|d| { let c = find(d, "C"); d.get_dictionary_mut(c).unwrap().set("Type", Object::Name(b"Page".to_vec())); }, "unexpected type");
    refused(&|d| { let root = d.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap(); d.get_dictionary_mut(root).unwrap().set("Type", Object::Name(b"Pages".to_vec())); }, "unexpected type");
    let mut ok = sample(); with_outline(&mut ok, &tree());
    let c = find(&ok, "C"); ok.get_dictionary_mut(c).unwrap().set("Type", Object::Name(b"Outline".to_vec()));
    assert_eq!(read(&ok).unwrap().len(), 7);
}

#[test]
fn titles_with_bidi_or_zero_width_characters_are_refused() {
    for bad in ["a\u{202E}b", "a\u{2066}b", "a\u{2069}b", "a\u{200B}b", "a\u{200F}b", "\u{FEFF}a", "a\u{202A}"] {
        assert!(clean_title(bad).unwrap_err().contains("invisible"), "{bad:?}");
    }
    assert_eq!(clean_title("  Caf\u{e9} \u{4e2d}\u{6587} ").unwrap(), "Caf\u{e9} \u{4e2d}\u{6587}");
}

#[test]
fn new_bookmarks_use_an_xyz_destination_that_keeps_the_zoom() {
    let mut document = sample();
    let mut edits = OutlineEdits::default(); edits.load(read(&document));
    op(&mut edits, BookmarkOp::Add { page: 2, title: "New".into(), after: None }).unwrap();
    write(&mut document, edits.pending().unwrap(), &document_pages).unwrap();
    let dest = document.get_dictionary(find(&document, "New")).unwrap().get(b"Dest").unwrap().as_array().unwrap().clone();
    assert_eq!(dest.len(), 5);
    assert_eq!(dest[0].as_reference().unwrap(), document_pages(2).unwrap());
    assert_eq!(dest[1].as_name().unwrap(), b"XYZ");
    assert!(dest[2..].iter().all(|object| matches!(object, Object::Null)));
}

#[test]
fn outline_items_that_alias_other_plain_objects_are_refused_and_those_objects_are_untouched() {
    let hostile = |attach: &dyn Fn(&mut Document, ObjectId)| {
        let mut document = sample(); with_outline(&mut document, &tree());
        let victim = document.new_object_id();
        document.objects.insert(victim, Object::Dictionary(dictionary! { "Title" => Object::string_literal("Real title"), "T" => Object::string_literal("field") }));
        attach(&mut document, victim);
        let root = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
        document.get_dictionary_mut(root).unwrap().set("First", Object::Reference(victim));
        let before: Vec<String> = document.objects.iter().filter(|(id, _)| **id != root).map(|(id, object)| format!("{id:?}{object:?}")).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        let mut edits = OutlineEdits::default(); edits.load(read(&document));
        let view = edits.view(&plan(6));
        assert!(!view.editable && view.reason.unwrap().contains("used by something else"));
        assert!(edits.apply(edits.version, BookmarkOp::Add { page: 0, title: "x".into(), after: None }, &plan(6)).is_err());
        assert!(edits.pending().is_none());
        let after: Vec<String> = document.objects.iter().filter(|(id, _)| **id != root).map(|(id, object)| format!("{id:?}{object:?}")).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        assert_eq!(before, after);
    };
    hostile(&|d, victim| { let form = d.new_object_id(); d.objects.insert(form, Object::Dictionary(dictionary! { "Fields" => vec![Object::Reference(victim)] })); d.catalog_mut().unwrap().set("AcroForm", Object::Reference(form)); });
    hostile(&|d, victim| { d.trailer.set("Info", Object::Reference(victim)); });
    hostile(&|d, victim| { let annotation = d.new_object_id(); d.objects.insert(annotation, Object::Dictionary(dictionary! { "Subtype" => "Link", "A" => Object::Reference(victim) })); let page = d.get_pages().values().next().copied().unwrap(); d.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(annotation)]); });
    hostile(&|d, victim| { d.catalog_mut().unwrap().set("OpenAction", Object::Reference(victim)); });
}

#[test]
fn a_direct_outline_root_does_not_trip_the_alias_check() {
    let mut document = sample(); with_outline(&mut document, &tree());
    let root_id = document.catalog().unwrap().get(b"Outlines").unwrap().as_reference().unwrap();
    let root = document.get_dictionary(root_id).unwrap().clone();
    document.objects.remove(&root_id);
    for (_, object) in document.objects.iter_mut() { if let Object::Dictionary(dict) = object { if dict.get(b"Parent").ok().and_then(|p| p.as_reference().ok()) == Some(root_id) { dict.remove(b"Parent"); } } }
    document.catalog_mut().unwrap().set("Outlines", Object::Dictionary(root));
    assert_eq!(read(&document).unwrap().len(), 7);
}
