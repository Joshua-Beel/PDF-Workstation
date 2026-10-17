use super::*;

struct Part { number: u32, header: u32, body: String, row: Option<String> }
fn part(number: u32, body: &str) -> Part { Part { number, header: number, body: body.into(), row: None } }
fn assemble(parts: &[Part], size: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.5\n".to_vec(); let mut rows: Vec<String> = (0..size).map(|n| if n == 0 { "0000000000 65535 f \n".into() } else { "0000000000 00001 f \n".to_string() }).collect();
    let mut xref_stream = String::new();
    for part in parts {
        let offset = out.len(); out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", part.header, part.body).as_bytes());
        if part.body.contains("/Type/XRef") { xref_stream = format!("/XRefStm {offset}"); }
        rows[part.number as usize] = part.row.clone().unwrap_or_else(|| format!("{offset:010} 00000 n \n"));
    }
    let start = out.len(); out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes()); for row in &rows { out.extend_from_slice(row.as_bytes()); }
    out.extend_from_slice(format!("trailer\n<</Size {size}/Root 1 0 R{xref_stream}>>\nstartxref\n{start}\n%%EOF\n").as_bytes()); out
}
fn base(extra: &str) -> Vec<Part> { vec![part(1, &format!("<</Type/Catalog/Pages 2 0 R{extra}>>")), part(2, "<</Type/Pages/Kids[3 0 R]/Count 1>>"), part(3, "<</Type/Page/Parent 2 0 R/MediaBox[0 0 10 10]>>")] }
fn load(bytes: &[u8]) -> Document { Document::load_mem_with_options(bytes, LoadOptions::with_max_decompressed_size(MAX_TEXT)).unwrap() }
fn check(bytes: &[u8]) -> Result<(), String> { check_faithful_load(&load(bytes), bytes) }
fn with_object_stream(mut parts: Vec<Part>, new_body: &str) -> Vec<u8> {
    let old = "<</Marker(old)>>"; parts.push(part(4, &format!("<</Type/ObjStm/N 1/First 4/Length {}>>\nstream\n5 0 {old}\nendstream", 4 + old.len())));
    parts.push(part(5, new_body)); assemble(&parts, 6)
}

#[test]
fn a_stale_object_stream_copy_standing_in_for_an_unparsable_newest_copy_is_refused() {
    let good = with_object_stream(base("/Extra 5 0 R"), "<</Marker(new)>>");
    let document = load(&good); assert_eq!(document.get_object((5, 0)).unwrap().as_dict().unwrap().get(b"Marker").unwrap().as_str().unwrap(), b"new"); assert!(check_faithful_load(&document, &good).is_ok());
    let bad = with_object_stream(base("/Extra 5 0 R"), "<</Marker(new)");
    let document = load(&bad); assert_eq!(document.get_object((5, 0)).unwrap().as_dict().unwrap().get(b"Marker").unwrap().as_str().unwrap(), b"old", "lopdf falls back to the older copy, which is the hazard under test");
    assert!(check_faithful_load(&document, &bad).unwrap_err().contains("older copy"));
    let session = EditSession::new(bad.clone(), 1); assert!(FormDocument::parse(&session).err().unwrap().contains("older copy"), "parse reaches the same refusal");
}

#[test]
fn unused_zero_offset_entries_and_free_entries_do_not_refuse() {
    let mut parts = base("/Extra 5 0 R/Gone 6 0 R"); parts.push(part(5, "<</Marker(new)>>"));
    let bytes = assemble(&parts, 7);
    let marker = b"0000000000 00001 f \n"; let mut rows = bytes.clone(); let at = rows.windows(marker.len()).rposition(|w| w == marker).unwrap(); rows[at..at + 20].copy_from_slice(b"0000000000 00000 n \n");
    assert!(check(&bytes).is_ok(), "a reference to a free entry is a null object, not a drop");
    assert!(check(&rows).is_ok(), "a `0000000000 00000 n` entry is an unused object that other viewers skip");
}

#[test]
fn a_header_that_disagrees_with_its_entry_and_an_object_that_fails_to_load_are_still_refused() {
    for body_header in [9, 5] {
        let mut parts = base("/Extra 5 0 R"); let mut moved = part(5, if body_header == 9 { "<</Marker(new)>>" } else { "<</Marker(new)" }); moved.header = body_header; parts.push(moved);
        let refusal = check(&assemble(&parts, 10)).unwrap_err(); assert!(refusal.contains("would drop"),"header {body_header}: {refusal}");
    }
    let mut orphan = base(""); orphan.push(part(5, "<</Marker(new)"));
    assert!(check(&assemble(&orphan, 6)).unwrap_err().contains("would drop"), "an unreferenced in-use entry that did not load is refused too");
}

#[test]
fn a_stream_whose_indirect_length_does_not_resolve_is_refused() {
    let stream = |length: &str| part(5, &format!("<</Length {length}>>\nstream\nabc\nendstream"));
    let build = |length: &str, with_length_object: bool| { let mut parts = base("/Extra 5 0 R"); parts.push(stream(length)); if with_length_object { parts.push(part(6, "3")); } assemble(&parts, 7) };
    let resolved = build("6 0 R", true); let document = load(&resolved); assert_eq!(document.get_object((5, 0)).unwrap().as_stream().unwrap().content, b"abc"); assert!(check_faithful_load(&document, &resolved).is_ok());
    let dangling = build("6 0 R", false); let document = load(&dangling); assert!(document.get_object((5, 0)).unwrap().as_stream().unwrap().content.is_empty(), "lopdf keeps the stream empty, which is the hazard under test");
    assert!(check_faithful_load(&document, &dangling).unwrap_err().contains("/Length"));
    let missing = build("(x)", false); assert!(check(&missing).unwrap_err().contains("/Length"));
    let empty_by_design = { let mut parts = base("/Extra 5 0 R"); parts.push(part(5, "<</Length 0>>\nstream\n\nendstream")); assemble(&parts, 6) }; assert!(check(&empty_by_design).is_ok());
}

fn object_stream_part(number: u32, object: &str) -> Part { let header = format!("{number} 0 "); part(4, &format!("<</Type/ObjStm/N 1/First {}/Length {}>>\nstream\n{header}{object}\nendstream", header.len(), header.len() + object.len())) }

#[test]
fn a_length_that_resolves_late_to_zero_while_data_follows_is_refused() {
    let mut parts = base("/Extra 5 0 R"); parts.push(object_stream_part(6, "0")); parts.push(part(5, "<</Length 6 0 R>>\nstream\nabc\nendstream"));
    let bytes = assemble(&parts, 7); let document = load(&bytes); assert!(document.get_object((5, 0)).unwrap().as_stream().unwrap().content.is_empty(), "the late zero length drops the data, which is the hazard under test");
    assert!(check_faithful_load(&document, &bytes).unwrap_err().contains("resolves to zero"));
}

#[test]
fn a_hybrid_xref_file_whose_object_stream_objects_are_declared_only_by_the_xref_stream_is_not_refused() {
    let mut parts = base("/Extra 6 0 R"); parts.push(object_stream_part(6, "<</Marker(stream)>>"));
    parts.push(part(7, "<</Type/XRef/W[1 2 1]/Index[6 1]/Size 8/Length 4>>\nstream\n\u{2}\0\u{4}\0\nendstream"));
    let bytes = assemble(&parts, 8); let text = String::from_utf8_lossy(&bytes); assert!(text.contains("/XRefStm "));
    let document = load(&bytes); assert!(document.objects.contains_key(&(6, 0)), "lopdf loads the object-stream object although the classic table lists it as free");
    assert!(check_faithful_load(&document, &bytes).is_ok(), "a normal hybrid file must open");
}

#[test]
fn a_stream_start_is_found_in_the_source_when_junk_precedes_the_header() {
    let build = |data: &str| { let mut parts = base("/Extra 5 0 R"); parts.push(object_stream_part(6, "0")); parts.push(part(5, &format!("<</Length 6 0 R>>\nstream\n{data}\nendstream")));
        let mut bytes = b"junk before the header\n".to_vec(); bytes.extend(assemble(&parts, 7)); bytes };
    let (empty, full) = (build(""), build("abc"));
    let document = load(&empty); assert!(document.get_object((5, 0)).unwrap().as_stream().unwrap().start_position.is_some());
    assert!(check_faithful_load(&document, &empty).is_ok(), "a genuinely empty stream is found at the right place despite the junk");
    assert!(check_faithful_load(&load(&full), &full).unwrap_err().contains("resolves to zero"));
}
