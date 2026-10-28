use super::*;
use crate::outline_edit::tests::{plan, sample};
use crate::page_labels::read_ranges;
use lopdf::Object;

const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/reportlab-page-labels.pdf");

fn loaded(document: &Document) -> LabelEdits {
    let mut edits = LabelEdits::default();
    edits.load(read_ranges(document, document.get_pages().len()), document.get_pages().len());
    edits
}
fn set(edits: &mut LabelEdits, start_page: usize, style: Option<Style>, prefix: &str, first: u64) -> Result<(), String> {
    let version = edits.version;
    edits.apply(version, LabelOp::Set { start_page, style, prefix: prefix.into(), first }, &plan(edits.page_count))
}
fn delete(edits: &mut LabelEdits, start_page: usize) -> Result<(), String> {
    let version = edits.version;
    edits.apply(version, LabelOp::Delete { start_page }, &plan(edits.page_count))
}
fn labels(edits: &LabelEdits) -> Vec<String> { expand_ranges(&edits.current, edits.page_count).unwrap() }
fn written(document: &Document) -> Vec<String> {
    let count = document.get_pages().len();
    let ranges = read_ranges(document, count).unwrap();
    if ranges.is_empty() { return (1..=count).map(|n| n.to_string()).collect(); }
    expand_ranges(&ranges, count).unwrap()
}
fn strs(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }

#[test]
fn ranges_can_be_defined_in_every_style_and_are_written_as_a_sorted_nums_array_covering_page_zero() {
    let mut document = sample();
    let mut edits = loaded(&document);
    assert!(!edits.changed() && edits.current.is_empty());
    set(&mut edits, 4, Some(Style::AlphaUpper), "App-", 3).unwrap();
    assert_eq!(edits.current[0], default_range(), "a first range after page 1 brings the default numbering for the pages before it");
    set(&mut edits, 0, Some(Style::RomanLower), "", 1).unwrap();
    set(&mut edits, 2, Some(Style::Decimal), "", 7).unwrap();
    set(&mut edits, 5, None, "Index", 99).unwrap();
    assert_eq!(labels(&edits), strs(&["i", "ii", "7", "8", "App-C", "Index"]), "a prefix-only range ignores the first number");
    assert!(edits.changed() && edits.pending().is_some());
    write(&mut document, edits.pending().unwrap()).unwrap();

    let tree_id = document.catalog().unwrap().get(b"PageLabels").unwrap().as_reference().unwrap();
    let nums = document.get_dictionary(tree_id).unwrap().get(b"Nums").unwrap().as_array().unwrap().clone();
    let keys: Vec<i64> = nums.chunks(2).map(|pair| pair[0].as_i64().unwrap()).collect();
    assert_eq!(keys, [0, 2, 4, 5], "sorted, unique, and page 0 first");
    let value = |index: usize| nums[index * 2 + 1].as_dict().unwrap().clone();
    assert_eq!(value(0).get(b"S").unwrap().as_name().unwrap(), b"r");
    assert!(!value(0).has(b"P") && !value(0).has(b"St"), "defaults are not written");
    assert_eq!(value(1).get(b"S").unwrap().as_name().unwrap(), b"D");
    assert_eq!(value(1).get(b"St").unwrap().as_i64().unwrap(), 7);
    assert_eq!(value(2).get(b"S").unwrap().as_name().unwrap(), b"A");
    assert_eq!(value(2).get(b"P").unwrap().as_str().unwrap(), b"App-");
    assert_eq!(value(2).get(b"St").unwrap().as_i64().unwrap(), 3);
    assert!(!value(3).has(b"S") && !value(3).has(b"St"), "a prefix-only range has /P and no /S or /St");
    assert_eq!(value(3).get(b"P").unwrap().as_str().unwrap(), b"Index");

    let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
    let reloaded = Document::load_mem(&bytes).unwrap();
    assert_eq!(read_ranges(&reloaded, 6).unwrap(), edits.current);
    assert_eq!(written(&reloaded), labels(&edits));
}

#[test]
fn deleting_a_range_extends_the_one_before_and_the_first_range_falls_back_to_the_default() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::RomanLower), "", 1).unwrap();
    set(&mut edits, 2, Some(Style::Decimal), "", 1).unwrap();
    set(&mut edits, 4, Some(Style::AlphaLower), "x", 1).unwrap();
    assert_eq!(labels(&edits), strs(&["i", "ii", "1", "2", "xa", "xb"]));
    delete(&mut edits, 2).unwrap();
    assert_eq!(labels(&edits), strs(&["i", "ii", "iii", "iv", "xa", "xb"]), "the middle range's pages continue the roman range");
    assert!(delete(&mut edits, 3).unwrap_err().contains("No page-label range"));
    delete(&mut edits, 0).unwrap();
    assert_eq!(labels(&edits), strs(&["1", "2", "3", "4", "xa", "xb"]), "page 1 gets the default numbering, later ranges stay");
    delete(&mut edits, 4).unwrap();
    assert!(edits.current.is_empty(), "only the default is left, which is the same as no labels");
    assert!(edits.changed() == false, "back to the source (no labels) is not a change");
    assert!(edits.pending().is_none());
}

#[test]
fn removing_the_last_labels_removes_page_labels_from_the_copy() {
    let mut document = Document::load_mem(FIXTURE).unwrap();
    let mut edits = loaded(&document);
    assert!(edits.current.len() > 3);
    while let Some(range) = edits.current.last().map(|range| range.start) { if edits.current.len() == 1 { delete(&mut edits, range).unwrap(); break; } delete(&mut edits, range).unwrap(); }
    assert!(edits.current.is_empty() && edits.changed());
    write(&mut document, edits.pending().unwrap()).unwrap();
    assert!(!document.catalog().unwrap().has(b"PageLabels"));
}

#[test]
fn untouched_labels_are_preserved_when_one_range_changes() {
    let mut document = Document::load_mem(FIXTURE).unwrap();
    let before = expand_ranges(&read_ranges(&document, 14).unwrap(), 14).unwrap();
    assert_eq!(before, strs(&["1", "2", "iv", "v", "I", "II", "AA", "BB", "bb", "cc", "Appendix", "Appendix", "N-5", "N-6"]));
    let mut edits = loaded(&document);
    assert!(edits.pending().is_none(), "opening the editor changes nothing");
    set(&mut edits, 2, Some(Style::RomanLower), "", 3).unwrap();
    let after = labels(&edits);
    assert_eq!(&after[..2], &before[..2]);
    assert_eq!(after[2], "iii");
    assert_eq!(&after[4..], &before[4..], "every later range keeps its numbering");
    write(&mut document, edits.pending().unwrap()).unwrap();
    let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
    assert_eq!(written(&Document::load_mem(&bytes).unwrap()), after);
}

#[test]
fn setting_the_default_on_a_document_without_labels_changes_nothing() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::Decimal), "", 1).unwrap();
    assert!(!edits.changed() && !edits.view(&plan(6)).can_undo, "no-op edits leave no history");
    set(&mut edits, 0, Some(Style::Decimal), "", 5).unwrap();
    assert_eq!(labels(&edits)[0], "5");
    delete(&mut edits, 0).unwrap();
    assert!(!edits.changed());
}

#[test]
fn invalid_ranges_are_refused_and_change_nothing() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::Decimal), "", 2).unwrap();
    let before = edits.current.clone(); let version = edits.version;
    for (page, style, prefix, first, wanted) in [
        (6, Some(Style::Decimal), "", 1u64, "out of range"),
        (1, Some(Style::Decimal), "", 0, "between 1"),
        (1, Some(Style::Decimal), "", 1_000_000_001, "between 1"),
        (1, None, "", 1, "style or enter a prefix"),
        (1, Some(Style::Decimal), "a\nb", 1, "control"),
        (1, Some(Style::Decimal), "a\u{202e}b", 1, "invisible"),
        (1, Some(Style::Decimal), "a\u{200b}b", 1, "invisible"),
        (1, Some(Style::Decimal), &"p".repeat(513), 1, "too long"),
        (1, Some(Style::Decimal), &"\u{3a9}".repeat(256), 1, "too long"),
        (1, Some(Style::RomanUpper), "", 999_999_999, "1024"),
    ] {
        let error = set(&mut edits, page, style, prefix, first).unwrap_err();
        assert!(error.contains(wanted), "{error}");
        assert_eq!(edits.current, before); assert_eq!(edits.version, version);
    }
    set(&mut edits, 1, Some(Style::Decimal), &"p".repeat(512), 1).unwrap();
    set(&mut edits, 2, Some(Style::Decimal), &"\u{3a9}".repeat(255), 1).unwrap();
}

#[test]
fn non_ascii_prefixes_round_trip_as_utf16() {
    let mut document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::Decimal), "\u{3a9}-\u{1f4d8} ", 1).unwrap();
    write(&mut document, edits.pending().unwrap()).unwrap();
    let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
    let reloaded = Document::load_mem(&bytes).unwrap();
    assert_eq!(written(&reloaded)[1], "\u{3a9}-\u{1f4d8} 2");
}

#[test]
fn undo_redo_and_stale_versions() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::RomanUpper), "", 1).unwrap();
    set(&mut edits, 3, Some(Style::Decimal), "", 1).unwrap();
    let stale = edits.version - 1;
    assert!(edits.apply(stale, LabelOp::Delete { start_page: 3 }, &plan(6)).unwrap_err().contains("changed"));
    let version = edits.version;
    edits.apply(version, LabelOp::Undo, &plan(6)).unwrap();
    assert_eq!(labels(&edits), strs(&["I", "II", "III", "IV", "V", "VI"]));
    let version = edits.version;
    edits.apply(version, LabelOp::Undo, &plan(6)).unwrap();
    assert!(!edits.changed());
    let version = edits.version;
    assert!(edits.apply(version, LabelOp::Undo, &plan(6)).is_err());
    edits.apply(version, LabelOp::Redo, &plan(6)).unwrap();
    set(&mut edits, 1, None, "Z", 1).unwrap();
    let version = edits.version;
    assert!(edits.apply(version, LabelOp::Redo, &plan(6)).is_err(), "a new edit clears redo");
    edits.discard();
    assert!(!edits.changed() && !edits.view(&plan(6)).can_undo);
}

#[test]
fn edits_are_accepted_only_while_the_page_order_is_the_source_order() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 1, Some(Style::Decimal), "", 1).unwrap();
    let mut reordered = plan(6); reordered.swap(0, 1);
    let mut shorter = plan(6); shorter.pop();
    for candidate in [reordered, shorter] {
        let version = edits.version;
        assert_eq!(edits.apply(version, LabelOp::Delete { start_page: 1 }, &candidate).unwrap_err(), STRUCTURE_REFUSAL);
        assert_eq!(edits.apply(version, LabelOp::Undo, &candidate).unwrap_err(), STRUCTURE_REFUSAL);
    }
    let mut rotated = plan(6); rotated[2].turns = 1;
    let version = edits.version;
    edits.apply(version, LabelOp::Delete { start_page: 1 }, &rotated).expect("rotation does not change the page order");
    let fresh = loaded(&document);
    let mut moved = plan(6); moved.swap(0, 1);
    let view = fresh.view(&moved);
    assert!(!view.editable && view.reason.as_deref() == Some(STRUCTURE_REFUSAL));
}

#[test]
fn extracting_pages_recomputes_the_ranges_so_each_page_keeps_its_label() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::RomanLower), "", 1).unwrap();
    set(&mut edits, 3, Some(Style::Decimal), "P", 1).unwrap();
    let all = labels(&edits);
    assert_eq!(all, strs(&["i", "ii", "iii", "P1", "P2", "P3"]));
    let subset = |pages: &[usize]| -> Vec<PageSpec> { pages.iter().map(|source| plan(6)[*source].clone()).collect() };
    for pages in [vec![1, 2, 4], vec![4, 5], vec![0, 1, 2, 3, 4, 5], vec![2, 3], vec![0, 3, 4]] {
        let ranges = output_ranges(&edits.current, 6, &subset(&pages)).unwrap();
        assert_eq!(ranges[0].start, 0, "page 0 is always covered");
        assert!(ranges.windows(2).all(|pair| pair[0].start < pair[1].start));
        assert_eq!(expand_ranges(&ranges, pages.len()).unwrap(), pages.iter().map(|page| all[*page].clone()).collect::<Vec<_>>(), "{pages:?}");
    }
    assert_eq!(output_ranges(&edits.current, 6, &plan(6)).unwrap(), edits.current, "the full document keeps the ranges as defined");
    let default_only = output_ranges(&[Range { start: 0, prefix: String::new(), style: Some(Style::Decimal), first: 1 }, Range { start: 2, prefix: String::new(), style: Some(Style::Decimal), first: 3 }], 6, &subset(&[1, 2, 3])).unwrap();
    assert!(default_only.is_empty() == false && default_only[0].first == 2);
}

#[test]
fn a_hostile_existing_number_tree_makes_editing_unavailable_without_touching_the_document() {
    let hostile = |build: &dyn Fn(&mut Document) -> Object, pages: usize| -> String {
        let mut document = sample();
        let tree = build(&mut document);
        document.catalog_mut().unwrap().set("PageLabels", tree);
        let before = format!("{:?}", document.objects);
        let result = read_ranges(&document, pages);
        assert_eq!(before, format!("{:?}", document.objects));
        let error = result.unwrap_err();
        let mut edits = LabelEdits::default(); edits.load(Err(error.clone()), pages);
        assert!(!edits.view(&plan(pages)).editable && edits.view(&plan(pages)).reason.as_deref() == Some(error.as_str()));
        assert!(set(&mut edits, 0, Some(Style::Decimal), "", 2).is_err() && !edits.changed());
        error
    };
    let leaf = |start: i64| Object::Dictionary(dictionary! { "Limits" => vec![start.into(), start.into()], "Nums" => vec![start.into(), Object::Dictionary(dictionary! { "S" => "D" })] });
    assert!(hostile(&|document| { let node = document.new_object_id(); document.objects.insert(node, Object::Dictionary(dictionary! { "Kids" => vec![Object::Reference(node)] })); Object::Reference(node) }, 6).contains("cycle"));
    assert!(hostile(&|document| { let shared = document.add_object(leaf(0)); Object::Dictionary(dictionary! { "Kids" => vec![Object::Reference(shared), Object::Reference(shared)] }) }, 6).contains("repeated node"));
    assert!(hostile(&|_| { let mut value = leaf(0); for _ in 0..20 { value = Object::Dictionary(dictionary! { "Limits" => vec![0.into(), 0.into()], "Kids" => vec![value] }); } value }, 6).contains("too deep"));
    assert!(hostile(&|_| Object::Dictionary(dictionary! { "Kids" => (0..2000).map(|index| leaf(index)).collect::<Vec<_>>() }), 2000).contains("too many nodes"));
    assert!(hostile(&|_| Object::Dictionary(dictionary! { "Nums" => (0..9000).flat_map(|index| [Object::Integer(index), Object::Dictionary(dictionary! { "S" => "D" })]).collect::<Vec<_>>() }), 9000).contains("too many page-label ranges"));
    assert!(hostile(&|_| Object::Dictionary(dictionary! { "Nums" => vec![1.into(), Object::Dictionary(dictionary! { "S" => "D" })] }), 6).contains("page 0"));
    assert!(hostile(&|_| Object::Dictionary(dictionary! { "Nums" => vec![0.into(), Object::Dictionary(dictionary! { "S" => "D", "St" => 2_000_000_000i64 }), 1.into(), Object::Dictionary(dictionary! { "S" => "R", "St" => 9_000_000_000_000i64 })] }), 6).contains("1024"));
    assert!(hostile(&|document| { let mut value = Object::Dictionary(dictionary! { "Nums" => vec![0.into(), Object::Dictionary(dictionary! { "S" => "D" })] }); for _ in 0..40 { value = Object::Reference(document.add_object(value)); } value }, 6).contains("deep"));
}

#[test]
fn page_label_ranges_beyond_the_page_count_or_range_cap_are_refused() {
    let mut document = sample();
    assert!(read_ranges(&document, 0).is_err() && read_ranges(&document, 70_000).is_err());
    let mut nums = Vec::new();
    for index in 0..5000 { nums.extend([Object::Integer(index), Object::Dictionary(dictionary! { "S" => "D" })]); }
    document.catalog_mut().unwrap().set("PageLabels", Object::Dictionary(dictionary! { "Nums" => nums }));
    assert!(read_ranges(&document, 5000).unwrap_err().contains("too many"));
    let mut edits = LabelEdits::default();
    let ranges: Vec<Range> = (0..MAX_RANGES).map(|start| Range { start, prefix: String::new(), style: Some(Style::Decimal), first: 1 + start as u64 }).collect();
    edits.load(Ok(ranges), 5000);
    let version = edits.version;
    let error = edits.apply(version, LabelOp::Set { start_page: MAX_RANGES, style: Some(Style::Decimal), prefix: String::new(), first: 1 }, &plan(5000)).unwrap_err();
    assert!(error.contains("at most 4096"), "{error}");
}

#[test]
fn view_reports_each_range_with_its_first_and_last_label() {
    let document = sample();
    let mut edits = loaded(&document);
    set(&mut edits, 0, Some(Style::RomanLower), "", 1).unwrap();
    set(&mut edits, 3, Some(Style::Decimal), "A-", 10).unwrap();
    let view = edits.view(&plan(6));
    assert!(view.editable && view.changed && view.can_undo && !view.can_redo && view.page_count == 6);
    let summary: Vec<_> = view.ranges.iter().map(|range| (range.start_page, range.end_page, range.first_label.as_str(), range.last_label.as_str())).collect();
    assert_eq!(summary, [(0, 2, "i", "iii"), (3, 5, "A-10", "A-12")]);
}
