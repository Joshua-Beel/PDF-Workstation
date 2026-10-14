use crate::editor::{write_new_file, EditSession};
use lopdf::{dictionary, Dictionary, Document, Object, ObjectId};
use std::{collections::HashSet, path::Path};

const MAX_PAGES: usize = 4_096;
const MAX_OUTPUT_BYTES: usize = 256 * 1024 * 1024;
const CATALOG_KEYS: &[&[u8]] = &[b"Type", b"Pages", b"Version", b"Metadata"];
const TREE_KEYS: &[&[u8]] = &[b"Type", b"Parent", b"Kids", b"Count", b"Resources", b"MediaBox", b"CropBox", b"Rotate"];
const PAGE_KEYS: &[&[u8]] = &[b"Type", b"Parent", b"Resources", b"Contents", b"MediaBox", b"CropBox", b"Rotate", b"BleedBox", b"TrimBox", b"ArtBox", b"UserUnit", b"Group", b"Metadata", b"LastModified"];

fn page_count(first: &EditSession, second: &EditSession) -> Result<usize, String> {
    if first.plan.is_empty() || second.plan.is_empty() { return Err("Choose two PDFs that each contain at least one page.".into()); }
    let count = first.plan.len().checked_add(second.plan.len()).ok_or("Combined page count is out of range.")?;
    if count > MAX_PAGES { return Err(format!("Combine is limited to {MAX_PAGES} pages in this build.")); }
    Ok(count)
}

fn output_size(size: usize) -> Result<(), String> {
    if size > MAX_OUTPUT_BYTES { return Err("The combined PDF exceeds the serialized output limit of 256 MiB. No output was written.".into()); }
    Ok(())
}

fn supported_keys(dictionary: &Dictionary, allowed: &[&[u8]], scope: &str) -> Result<(), String> {
    for (key, _) in dictionary.iter() {
        if !allowed.contains(&key.as_slice()) {
            return Err(format!("Combine cannot preserve {scope} feature /{} in this build. The source PDFs are unchanged.", String::from_utf8_lossy(key)));
        }
    }
    Ok(())
}

fn supported_shape(document: &Document) -> Result<(), String> {
    let catalog = document.catalog().map_err(|error| error.to_string())?;
    supported_keys(catalog, CATALOG_KEYS, "catalog")?;
    let root = catalog.get(b"Pages").and_then(Object::as_reference).map_err(|error| error.to_string())?;
    let mut remaining = vec![(root, None)];
    let mut visited = HashSet::new();
    let mut nodes = Vec::new();
    while let Some((id, parent)) = remaining.pop() {
        if !visited.insert(id) { return Err("The PDF repeats a page or page-tree object, or contains a cycle. Combine is blocked to preserve it.".into()); }
        let node = document.get_dictionary(id).map_err(|error| error.to_string())?;
        match (parent, node.get(b"Parent").ok()) {
            (None, None) => (),
            (Some(expected), Some(value)) if value.as_reference().ok() == Some(expected) => (),
            _ => return Err("The PDF has inconsistent page-tree parents. Combine is blocked to preserve it.".into()),
        }
        match node.get(b"Type").and_then(Object::as_name).map_err(|error| error.to_string())? {
            b"Pages" => {
                supported_keys(node, TREE_KEYS, "page-tree")?;
                let kids = document.dereference(node.get(b"Kids").map_err(|error| error.to_string())?).map_err(|error| error.to_string())?.1.as_array().map_err(|error| error.to_string())?;
                let kids = kids.iter().map(|kid| kid.as_reference().map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?;
                for kid in &kids { remaining.push((*kid, Some(id))); }
                nodes.push((id, Some(kids)));
            }
            b"Page" => { supported_keys(node, PAGE_KEYS, "page")?; nodes.push((id, None)); },
            _ => return Err("The PDF has an unsupported page-tree object. Combine is blocked to preserve it.".into()),
        }
    }
    let mut counts = std::collections::HashMap::<ObjectId, usize>::new();
    for (id, kids) in nodes.iter().rev() {
        let count = match kids {
            Some(kids) => {
                let count = kids.iter().try_fold(0usize, |count, kid| count.checked_add(counts[kid]).ok_or("The page-tree count is out of range."))?;
                let stated = document.get_dictionary(*id).map_err(|error| error.to_string())?.get(b"Count").and_then(Object::as_i64).map_err(|error| error.to_string())?;
                if stated < 0 || stated as u64 != count as u64 { return Err("The PDF has inconsistent page-tree counts. Combine is blocked to preserve it.".into()); }
                count
            }
            None => 1,
        };
        counts.insert(*id, count);
    }
    visited.insert(document.trailer.get(b"Root").and_then(Object::as_reference).map_err(|error| error.to_string())?);
    for (id, _) in &nodes {
        for (key, value) in document.get_dictionary(*id).map_err(|error| error.to_string())?.iter() {
            if key != b"Parent" && key != b"Kids" { opaque_graph(document, value, &visited)?; }
        }
    }
    for value in [catalog.get(b"Metadata").ok(), document.trailer.get(b"Info").ok()].into_iter().flatten() { opaque_graph(document, value, &visited)?; }
    Ok(())
}

fn opaque_graph(document: &Document, value: &Object, page_graph: &HashSet<ObjectId>) -> Result<(), String> {
    let mut remaining = vec![value]; let mut visited = HashSet::new();
    while let Some(value) = remaining.pop() {
        match value {
            Object::Reference(id) => {
                if page_graph.contains(id) { return Err("The PDF contains a content/resource or metadata reference to its old catalog or page tree. Combine cannot preserve that reference in this build.".into()); }
                if visited.insert(*id) { remaining.push(document.get_object(*id).map_err(|error| error.to_string())?); }
            }
            Object::Array(values) => remaining.extend(values),
            Object::Dictionary(values) => remaining.extend(values.iter().map(|(_, value)| value)),
            Object::Stream(stream) => remaining.extend(stream.dict.iter().map(|(_, value)| value)),
            _ => (),
        }
    }
    Ok(())
}

fn checked_document(session: &EditSession, label: &str) -> Result<Document, String> {
    let bytes = session.export(None).map_err(|error| format!("{label} PDF: {error}"))?;
    let original = crate::pdf_load::load(&session.source).map_err(|error| format!("{label} PDF: {error}"))?;
    supported_shape(&original).map_err(|error| format!("{label} PDF: {error}"))?;
    let document = Document::load_mem(&bytes).map_err(|error| format!("{label} PDF: {error}"))?;
    supported_shape(&document).map_err(|error| format!("{label} PDF: {error}"))?;
    Ok(document)
}

pub fn validate_sources(first: &EditSession, second: &EditSession) -> Result<usize, String> {
    let count = page_count(first, second)?;
    checked_document(first, "First")?; checked_document(second, "Second")?;
    Ok(count)
}

pub fn validate_insertion(target: &EditSession, donor: &EditSession, at: usize) -> Result<usize, String> {
    if at > target.plan.len() { return Err("Choose an insertion position within the target PDF, including its beginning or end.".into()); }
    let count = page_count(target, donor)?;
    checked_document(target, "Target")?; checked_document(donor, "Donor")?;
    Ok(count)
}

fn replacement_end(pages: usize, start: usize, count: usize) -> Result<usize, String> {
    start.checked_add(count).filter(|end| count > 0 && *end <= pages)
        .ok_or_else(|| "Choose a nonempty contiguous replacement range within the target PDF.".into())
}

fn replacement_page_count(target: &EditSession, donor: &EditSession, start: usize, count: usize) -> Result<usize, String> {
    replacement_end(target.plan.len(), start, count)?;
    // The existing aggregate input limit applies even when replacement makes a smaller output.
    Ok(page_count(target, donor)? - count)
}

pub fn validate_replacement(target: &EditSession, donor: &EditSession, start: usize, count: usize) -> Result<usize, String> {
    let output_count = replacement_page_count(target, donor, start, count)?;
    checked_document(target, "Target")?; checked_document(donor, "Donor")?;
    Ok(output_count)
}

fn inherited(document: &Document, mut id: ObjectId, key: &[u8]) -> Result<Option<Object>, String> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(id) { return Err("The page tree contains a cycle.".into()); }
        let node = document.get_dictionary(id).map_err(|error| error.to_string())?;
        if let Ok(value) = node.get(key) { return Ok(Some(document.dereference(value).map_err(|error| error.to_string())?.1.clone())); }
        match node.get(b"Parent") {
            Ok(parent) => id = parent.as_reference().map_err(|error| error.to_string())?,
            Err(_) => return Ok(None),
        }
    }
}

fn flatten_pages(document: &mut Document) -> Result<Vec<ObjectId>, String> {
    let pages = document.get_pages().into_values().collect::<Vec<_>>();
    for id in &pages {
        let attributes = [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"].into_iter()
            .map(|key| Ok((key, inherited(document, *id, key)?))).collect::<Result<Vec<_>, String>>()?;
        let page = document.get_dictionary_mut(*id).map_err(|error| error.to_string())?;
        for (key, value) in attributes { if let Some(value) = value { page.set(key, value); } }
    }
    Ok(pages)
}

fn version(document: &Document) -> Result<(u8, u8), String> {
    let parse = |value: &str| -> Result<(u8, u8), String> {
        match value {
            "1.0" => Ok((1, 0)), "1.1" => Ok((1, 1)), "1.2" => Ok((1, 2)), "1.3" => Ok((1, 3)),
            "1.4" => Ok((1, 4)), "1.5" => Ok((1, 5)), "1.6" => Ok((1, 6)), "1.7" => Ok((1, 7)), "2.0" => Ok((2, 0)),
            _ => Err("Combine cannot preserve this PDF version in this build.".into()),
        }
    };
    let header = parse(&document.version)?;
    let declared = document.catalog().map_err(|error| error.to_string())?.get(b"Version").ok()
        .map(|value| {
            let name = document.dereference(value).map_err(|error| error.to_string())?.1.as_name().map_err(|error| error.to_string())?;
            parse(std::str::from_utf8(name).map_err(|error| error.to_string())?)
        }).transpose()?;
    Ok(declared.map_or(header, |declared| header.max(declared)))
}

#[derive(Clone, Copy)]
pub(crate) enum CopyOperation { Combine, Insert { at: usize }, Replace { start: usize, count: usize } }
impl CopyOperation {
    pub(crate) fn name(self) -> &'static str {
        match self { Self::Combine => "Combine", Self::Insert { .. } => "Insert Pages", Self::Replace { .. } => "Replace Pages" }
    }
}

pub fn assemble(first: &EditSession, second: &EditSession) -> Result<Vec<u8>, String> {
    assemble_sequence(first, second, CopyOperation::Combine)
}

pub fn assemble_insertion(target: &EditSession, donor: &EditSession, at: usize) -> Result<Vec<u8>, String> {
    if at > target.plan.len() { return Err("Choose an insertion position within the target PDF, including its beginning or end.".into()); }
    assemble_sequence(target, donor, CopyOperation::Insert { at })
}

pub fn assemble_replacement(target: &EditSession, donor: &EditSession, start: usize, count: usize) -> Result<Vec<u8>, String> {
    assemble_sequence(target, donor, CopyOperation::Replace { start, count })
}

fn assemble_sequence(first: &EditSession, second: &EditSession, sequence: CopyOperation) -> Result<Vec<u8>, String> {
    let count = match sequence {
        CopyOperation::Replace { start, count } => replacement_page_count(first, second, start, count)?,
        _ => page_count(first, second)?,
    };
    let labels = if matches!(sequence, CopyOperation::Combine) { ("First", "Second") } else { ("Target", "Donor") };
    let mut first = checked_document(first, labels.0)?;
    let mut second = checked_document(second, labels.1)?;
    let version = version(&first)?.max(version(&second)?);
    let objects = first.objects.len().checked_add(second.objects.len()).ok_or("The combined object graph is too large.")?;
    if objects > u32::MAX as usize - 3 { return Err("The combined object graph is too large.".into()); }
    first.renumber_objects_with(1);
    let start = first.max_id.checked_add(1).ok_or("The combined object graph is too large.")?;
    second.renumber_objects_with(start);
    let info = first.trailer.get(b"Info").ok().cloned();
    let metadata = first.catalog().map_err(|error| error.to_string())?.get(b"Metadata").ok().cloned();
    // Metadata ownership follows the target/first source independently of the displayed page order.
    let mut pages = flatten_pages(&mut first)?;
    let donor = flatten_pages(&mut second)?;
    match sequence {
        CopyOperation::Combine => pages.extend(donor),
        CopyOperation::Insert { at } => {
            if at > pages.len() { return Err("Insertion position differs from the exported target plan.".into()); }
            let suffix = pages.split_off(at);
            pages.extend(donor); pages.extend(suffix);
        }
        CopyOperation::Replace { start, count } => {
            let end = replacement_end(pages.len(), start, count)?;
            let suffix = pages.split_off(end);
            pages.truncate(start); pages.extend(donor); pages.extend(suffix);
        }
    }
    if pages.len() != count { return Err("Combined page count differs from the edit plans.".into()); }
    let mut output = Document::with_version(format!("{}.{}", version.0, version.1));
    output.max_id = second.max_id;
    output.objects.extend(first.objects); output.objects.extend(second.objects);
    if output.objects.len() != objects { return Err("The combined object graphs overlap. No output was written.".into()); }
    let root = output.new_object_id();
    for id in &pages { output.get_dictionary_mut(*id).map_err(|error| error.to_string())?.set("Parent", root); }
    output.objects.insert(root, dictionary! { "Type" => "Pages", "Count" => count as i64, "Kids" => pages.into_iter().map(Object::Reference).collect::<Vec<_>>() }.into());
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => root };
    if let Some(metadata) = metadata { catalog.set("Metadata", metadata); }
    let catalog = output.add_object(catalog); output.trailer.set("Root", catalog);
    if let Some(info) = info { output.trailer.set("Info", info); }
    crate::pdf_load::prune_unreachable(&mut output);
    let mut bytes = Vec::new(); output.save_to(&mut bytes).map_err(|error| error.to_string())?;
    output_size(bytes.len())?;
    Ok(bytes)
}

pub fn prepare_and_write(first: &EditSession, second: &EditSession, path: &Path, validate: impl FnOnce(&[u8], usize) -> Result<(), String>) -> Result<Vec<u8>, String> {
    if path.symlink_metadata().is_ok() { return Err("That file already exists. Choose a new filename; Combine never overwrites an existing file.".into()); }
    let bytes = assemble(first, second)?;
    validate(&bytes, page_count(first, second)?).map_err(|error| format!("Combined output validation failed: {error}"))?;
    write_new_file(path, &bytes)?;
    Ok(bytes)
}

pub fn prepare_insertion_and_write(target: &EditSession, donor: &EditSession, at: usize, path: &Path, validate: impl FnOnce(&[u8], usize) -> Result<(), String>) -> Result<Vec<u8>, String> {
    if path.symlink_metadata().is_ok() { return Err("That file already exists. Choose a new filename; Insert Pages never overwrites an existing file.".into()); }
    let bytes = assemble_insertion(target, donor, at)?;
    validate(&bytes, page_count(target, donor)?).map_err(|error| format!("Inserted output validation failed: {error}"))?;
    write_new_file(path, &bytes)?;
    Ok(bytes)
}

pub fn prepare_replacement_and_write(target: &EditSession, donor: &EditSession, start: usize, count: usize, path: &Path, validate: impl FnOnce(&[u8], usize) -> Result<(), String>) -> Result<Vec<u8>, String> {
    if path.symlink_metadata().is_ok() { return Err("That file already exists. Choose a new filename; Replace Pages never overwrites an existing file.".into()); }
    let expected = replacement_page_count(target, donor, start, count)?;
    let bytes = assemble_replacement(target, donor, start, count)?;
    validate(&bytes, expected).map_err(|error| format!("Replaced output validation failed: {error}"))?;
    write_new_file(path, &bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{CropBox, PageEdit};
    use lopdf::Stream;
    fn sample() -> Vec<u8> { std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }
    fn source(title: &str, font: &str) -> Vec<u8> {
        let mut document = Document::load_mem(&sample()).unwrap();
        let info = document.add_object(dictionary! { "Title" => Object::string_literal(title), "Author" => Object::string_literal(format!("author-{title}")) });
        document.trailer.set("Info", info);
        let metadata = document.add_object(Stream::new(dictionary! { "Type" => "Metadata", "Subtype" => "XML" }, format!("<fixture>{title}</fixture>").into_bytes()));
        document.catalog_mut().unwrap().set("Metadata", metadata);
        for object in document.objects.values_mut() { if let Ok(dictionary) = object.as_dict_mut() { if dictionary.get(b"Type").is_ok_and(|kind| kind.as_name().is_ok_and(|kind| kind == b"Font")) { dictionary.set("BaseFont", Object::Name(font.as_bytes().to_vec())); } } }
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); bytes
    }
    #[test]
    fn replace_first_middle_last_whole_and_differing_counts_keep_target_metadata_and_source_history() {
        let target_bytes = source("target-title", "Helvetica"); let donor_bytes = source("donor-title", "Courier");
        let mut target = EditSession::new(target_bytes.clone(), 6); let mut donor = EditSession::new(donor_bytes.clone(), 6);
        target.apply(PageEdit::Move { from: 0, to: 2 }).unwrap(); target.mark_saved();
        target.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        target.apply(PageEdit::Crop { page: 1, crop: CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 } }).unwrap();
        donor.apply(PageEdit::Delete { pages: vec![1, 3] }).unwrap(); donor.apply(PageEdit::Move { from: 3, to: 0 }).unwrap();
        donor.apply(PageEdit::Rotate { pages: vec![1], clockwise: false }).unwrap();
        let target_plan = target.plan.clone(); let donor_plan = donor.plan.clone();
        let originals = [Document::load_mem(&target_bytes).unwrap(), Document::load_mem(&donor_bytes).unwrap()];
        let cases = [
            (0,2,vec![(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,0,0),(0,3,0),(0,4,0),(0,5,0)]),
            (2,2,vec![(0,1,90),(0,2,0),(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,4,0),(0,5,0)]),
            (4,2,vec![(0,1,90),(0,2,0),(0,0,0),(0,3,0),(1,5,0),(1,0,270),(1,2,0),(1,4,0)]),
            (0,6,vec![(1,5,0),(1,0,270),(1,2,0),(1,4,0)]),
            (0,5,vec![(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,5,0)]),
            (1,4,vec![(0,1,90),(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,5,0)]),
        ];
        for (start, count, expected) in cases {
            assert_eq!(validate_replacement(&target, &donor, start, count).unwrap(), expected.len());
            let output = Document::load_mem(&assemble_replacement(&target, &donor, start, count).unwrap()).unwrap();
            assert_eq!(output.get_pages().len(), expected.len());
            let mut target_fonts = HashSet::new(); let mut donor_fonts = HashSet::new();
            for (index, id) in output.get_pages().into_values().enumerate() {
                let (owner, source_page, rotation) = expected[index];
                assert_eq!(output.get_page_content(id), originals[owner].get_page_content(originals[owner].get_pages()[&(source_page + 1)]));
                assert_eq!(output.get_dictionary(id).unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), rotation);
                let resources = inherited(&output, id, b"Resources").unwrap().unwrap();
                let fonts = output.dereference(resources.as_dict().unwrap().get(b"Font").unwrap()).unwrap().1.as_dict().unwrap();
                for (_, reference) in fonts.iter() {
                    let font = output.dereference(reference).unwrap().1.as_dict().unwrap();
                    assert_eq!(font.get(b"BaseFont").unwrap().as_name().unwrap(), if owner == 0 { b"Helvetica".as_slice() } else { b"Courier".as_slice() });
                    if owner == 0 { target_fonts.insert(reference.as_reference().unwrap()); } else { donor_fonts.insert(reference.as_reference().unwrap()); }
                }
                if owner == 0 && source_page == 2 { assert_eq!(output.get_dictionary(id).unwrap().get(b"CropBox").unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<_>>(), vec![30.0,60.0,590.0,780.0]); }
            }
            assert!(target_fonts.is_disjoint(&donor_fonts));
            if count == 6 { assert!(target_fonts.is_empty()); }
            else {
                let original_fonts = originals[0].objects.values().filter(|value| value.as_dict().is_ok_and(|value| value.get(b"Type").is_ok_and(|value| value.as_name().is_ok_and(|name| name == b"Font")))).count();
                assert_eq!(target_fonts.len(), original_fonts, "Retained target pages must share one resource graph");
            }
            let info = output.dereference(output.trailer.get(b"Info").unwrap()).unwrap().1.as_dict().unwrap(); assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"target-title");
            let xmp = output.dereference(output.catalog().unwrap().get(b"Metadata").unwrap()).unwrap().1.as_stream().unwrap(); assert_eq!(xmp.content, b"<fixture>target-title</fixture>");
            assert!(!output.trailer.has(b"ID"));
        }
        assert_eq!(target.source, target_bytes); assert_eq!(donor.source, donor_bytes); assert_eq!(target.plan, target_plan); assert_eq!(donor.plan, donor_plan);
        assert_eq!((target.revision, donor.revision), (3,3)); assert!(target.dirty() && donor.dirty() && target.can_undo() && donor.can_undo());
        target.apply(PageEdit::Undo).unwrap(); target.apply(PageEdit::Undo).unwrap(); assert!(!target.dirty());
    }
    #[test]
    fn replace_invalid_ranges_removed_page_features_input_limits_and_publish_failures_reject() {
        let target = EditSession::new(sample(), 6); let donor = EditSession::new(sample(), 6);
        for (start,count) in [(0,0),(6,1),(5,2),(usize::MAX,1),(1,usize::MAX)] { assert!(validate_replacement(&target, &donor, start, count).unwrap_err().contains("range")); }
        let mut unsupported = Document::load_mem(&sample()).unwrap(); let removed = unsupported.get_pages()[&3]; unsupported.get_dictionary_mut(removed).unwrap().set("Annots", Vec::<Object>::new());
        let mut bytes = Vec::new(); unsupported.save_to(&mut bytes).unwrap(); let unsupported = EditSession::new(bytes, 6);
        for (start,count) in [(2,1),(0,6)] { assert!(assemble_replacement(&unsupported, &donor, start, count).unwrap_err().contains("Annots"), "Removed pages must still satisfy full-input preservation guards"); }
        assert!(assemble_replacement(&target, &EditSession::new(b"not a PDF".to_vec(), 6), 0, 6).is_err());
        let mut signed = Document::load_mem(&sample()).unwrap(); signed.add_object(dictionary! { "Type" => "Sig", "ByteRange" => vec![0.into(),1.into(),2.into(),3.into()] });
        let mut bytes = Vec::new(); signed.save_to(&mut bytes).unwrap(); assert!(assemble_replacement(&EditSession::new(bytes, 6), &donor, 0, 6).unwrap_err().contains("Signed"));
        let mut large = EditSession::new(sample(), 6); large.plan.resize(MAX_PAGES - 5, large.plan[0].clone());
        assert!(replacement_page_count(&large, &donor, 0, large.plan.len()).unwrap_err().contains("4096"), "Smaller output must not bypass the input cap");
        large.plan.pop(); assert_eq!(replacement_page_count(&large, &donor, 0, large.plan.len()).unwrap(), 6);
        output_size(MAX_OUTPUT_BYTES).unwrap(); assert!(output_size(MAX_OUTPUT_BYTES + 1).is_err());
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("replaced.pdf");
        assert!(prepare_replacement_and_write(&target, &donor, 2, 2, &path, |_,_| Err("Injected validation failure".into())).unwrap_err().contains("Injected"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        assert!(prepare_replacement_and_write(&target, &donor, 2, 2, &path, |_,expected| { assert_eq!(expected,10); std::fs::write(&path,b"racing file").unwrap(); Ok(()) }).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"racing file");
        assert!(prepare_replacement_and_write(&target, &donor, 2, 2, &path, |_,_| panic!("Existing output must reject before validation")).is_err());
    }
    #[test]
    fn insert_current_order_beginning_middle_end_target_metadata_and_resources_preserve_sessions() {
        let target_bytes = source("target-title", "Helvetica"); let donor_bytes = source("donor-title", "Courier");
        let mut target = EditSession::new(target_bytes.clone(), 6); let mut donor = EditSession::new(donor_bytes.clone(), 6);
        target.apply(PageEdit::Move { from: 0, to: 2 }).unwrap(); target.mark_saved();
        target.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        target.apply(PageEdit::Crop { page: 1, crop: CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 } }).unwrap();
        donor.apply(PageEdit::Delete { pages: vec![1, 3] }).unwrap(); donor.apply(PageEdit::Move { from: 3, to: 0 }).unwrap();
        donor.apply(PageEdit::Rotate { pages: vec![1], clockwise: false }).unwrap();
        let target_plan = target.plan.clone(); let donor_plan = donor.plan.clone();
        let originals = [Document::load_mem(&target_bytes).unwrap(), Document::load_mem(&donor_bytes).unwrap()];
        let cases = [
            (0, vec![(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,1,90),(0,2,0),(0,0,0),(0,3,0),(0,4,0),(0,5,0)]),
            (2, vec![(0,1,90),(0,2,0),(1,5,0),(1,0,270),(1,2,0),(1,4,0),(0,0,0),(0,3,0),(0,4,0),(0,5,0)]),
            (6, vec![(0,1,90),(0,2,0),(0,0,0),(0,3,0),(0,4,0),(0,5,0),(1,5,0),(1,0,270),(1,2,0),(1,4,0)]),
        ];
        for (at, expected) in cases {
            assert_eq!(validate_insertion(&target, &donor, at).unwrap(), 10);
            let output = Document::load_mem(&assemble_insertion(&target, &donor, at).unwrap()).unwrap();
            assert_eq!(output.get_pages().len(), 10);
            let mut target_fonts = HashSet::new(); let mut donor_fonts = HashSet::new();
            for (index, id) in output.get_pages().into_values().enumerate() {
                let (owner, source_page, rotation) = expected[index];
                assert_eq!(output.get_page_content(id), originals[owner].get_page_content(originals[owner].get_pages()[&(source_page + 1)]));
                assert_eq!(output.get_dictionary(id).unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), rotation);
                let resources = inherited(&output, id, b"Resources").unwrap().unwrap();
                let fonts = output.dereference(resources.as_dict().unwrap().get(b"Font").unwrap()).unwrap().1.as_dict().unwrap();
                for (_, reference) in fonts.iter() {
                    let font = output.dereference(reference).unwrap().1.as_dict().unwrap();
                    assert_eq!(font.get(b"BaseFont").unwrap().as_name().unwrap(), if owner == 0 { b"Helvetica".as_slice() } else { b"Courier".as_slice() });
                    if owner == 0 { target_fonts.insert(reference.as_reference().unwrap()); } else { donor_fonts.insert(reference.as_reference().unwrap()); }
                }
                if owner == 0 && source_page == 2 { assert_eq!(output.get_dictionary(id).unwrap().get(b"CropBox").unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<_>>(), vec![30.0, 60.0, 590.0, 780.0]); }
            }
            assert!(target_fonts.is_disjoint(&donor_fonts));
            let original_fonts: HashSet<_> = originals[0].objects.iter().filter_map(|(id, value)| value.as_dict().ok().filter(|value| value.get(b"Type").is_ok_and(|value| value.as_name().is_ok_and(|name| name == b"Font"))).map(|_| *id)).collect();
            assert_eq!(target_fonts.len(), original_fonts.len(), "Target prefix and suffix must reuse the same font graph");
            let info = output.dereference(output.trailer.get(b"Info").unwrap()).unwrap().1.as_dict().unwrap();
            assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"target-title");
            let xmp = output.dereference(output.catalog().unwrap().get(b"Metadata").unwrap()).unwrap().1.as_stream().unwrap();
            assert_eq!(xmp.content, b"<fixture>target-title</fixture>"); assert!(!output.trailer.has(b"ID"));
        }
        assert_eq!(target.source, target_bytes); assert_eq!(donor.source, donor_bytes);
        assert_eq!(target.plan, target_plan); assert_eq!(donor.plan, donor_plan); assert_eq!((target.revision, donor.revision), (3,3));
        assert!(target.dirty() && donor.dirty() && target.can_undo() && donor.can_undo());
        target.apply(PageEdit::Undo).unwrap(); target.apply(PageEdit::Undo).unwrap(); assert!(!target.dirty());
    }
    #[test]
    fn insert_invalid_boundaries_guards_validation_failure_and_racing_output_never_publish() {
        let target = EditSession::new(sample(), 6); let donor = EditSession::new(sample(), 6);
        assert!(assemble_insertion(&target, &donor, 7).unwrap_err().contains("position"));
        assert!(validate_insertion(&target, &donor, usize::MAX).unwrap_err().contains("position"));
        assert!(assemble_insertion(&target, &EditSession::new(b"not a PDF".to_vec(), 6), 3).is_err());
        let mut signed = Document::load_mem(&sample()).unwrap(); signed.add_object(dictionary! { "Type" => "Sig", "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] });
        let mut signed_bytes = Vec::new(); signed.save_to(&mut signed_bytes).unwrap();
        assert!(assemble_insertion(&target, &EditSession::new(signed_bytes, 6), 0).unwrap_err().contains("Signed"));
        let mut unsupported = Document::load_mem(&sample()).unwrap(); unsupported.catalog_mut().unwrap().set("Outlines", dictionary! {});
        let mut bytes = Vec::new(); unsupported.save_to(&mut bytes).unwrap();
        assert!(assemble_insertion(&EditSession::new(bytes, 6), &donor, 0).unwrap_err().contains("Outlines"));
        let mut large = EditSession::new(sample(), 6); large.plan.resize(MAX_PAGES - 5, large.plan[0].clone());
        assert!(validate_insertion(&target, &large, 0).unwrap_err().contains("4096"));
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("inserted.pdf");
        assert!(prepare_insertion_and_write(&target, &donor, 2, &path, |_, _| Err("Injected validation failure".into())).unwrap_err().contains("Injected"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        assert!(prepare_insertion_and_write(&target, &donor, 2, &path, |_, _| { std::fs::write(&path, b"racing file").unwrap(); Ok(()) }).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"racing file");
        assert!(prepare_insertion_and_write(&target, &donor, 2, &path, |_, _| panic!("Existing output must reject before validation")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"racing file");
    }
    #[test]
    fn combine_current_order_crop_rotation_resource_collisions_and_first_metadata_preserve_sessions() {
        let first_bytes = source("first-title", "Helvetica"); let second_bytes = source("second-title", "Courier");
        let mut first = EditSession::new(first_bytes.clone(), 6); let mut second = EditSession::new(second_bytes.clone(), 6);
        first.apply(PageEdit::Move { from: 0, to: 2 }).unwrap(); first.mark_saved();
        first.apply(PageEdit::Crop { page: 0, crop: CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 } }).unwrap();
        first.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        second.apply(PageEdit::Delete { pages: vec![1, 3] }).unwrap(); second.apply(PageEdit::Rotate { pages: vec![0], clockwise: false }).unwrap();
        let first_plan = first.plan.clone(); let second_plan = second.plan.clone();
        let output = Document::load_mem(&assemble(&first, &second).unwrap()).unwrap();
        assert_eq!(output.get_pages().len(), 10);
        let first_original = Document::load_mem(&first_bytes).unwrap(); let second_original = Document::load_mem(&second_bytes).unwrap();
        let expected = [(1usize, 90i64), (2, 0), (0, 0), (3, 0), (4, 0), (5, 0), (0, 270), (2, 0), (4, 0), (5, 0)];
        for (index, id) in output.get_pages().into_values().enumerate() {
            let (source_page, rotation) = expected[index]; let original = if index < 6 { &first_original } else { &second_original };
            assert_eq!(output.get_page_content(id), original.get_page_content(original.get_pages()[&(source_page as u32 + 1)]));
            assert_eq!(output.get_dictionary(id).unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), rotation);
            let resources = inherited(&output, id, b"Resources").unwrap().unwrap();
            let fonts = output.dereference(resources.as_dict().unwrap().get(b"Font").unwrap()).unwrap().1.as_dict().unwrap();
            assert!(!fonts.is_empty());
            for (_, font) in fonts.iter() {
                let font = output.dereference(font).unwrap().1.as_dict().unwrap();
                assert_eq!(font.get(b"BaseFont").unwrap().as_name().unwrap(), if index < 6 { b"Helvetica".as_slice() } else { b"Courier".as_slice() });
            }
            if index == 0 { assert_eq!(output.get_dictionary(id).unwrap().get(b"CropBox").unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<_>>(), vec![30.0, 60.0, 590.0, 780.0]); }
        }
        let info = output.dereference(output.trailer.get(b"Info").unwrap()).unwrap().1.as_dict().unwrap();
        assert_eq!(info.get(b"Title").unwrap().as_str().unwrap(), b"first-title");
        let xmp = output.dereference(output.catalog().unwrap().get(b"Metadata").unwrap()).unwrap().1.as_stream().unwrap(); assert_eq!(xmp.content, b"<fixture>first-title</fixture>");
        assert!(!output.trailer.has(b"ID"));
        assert_eq!(first.source, first_bytes); assert_eq!(second.source, second_bytes);
        assert_eq!(first.plan, first_plan); assert_eq!(second.plan, second_plan); assert_eq!((first.revision, second.revision), (3, 2));
        assert!(first.dirty() && second.dirty() && first.can_undo() && second.can_undo());
        first.apply(PageEdit::Undo).unwrap(); first.apply(PageEdit::Undo).unwrap(); assert!(!first.dirty());
    }
    #[test]
    fn combine_unknown_features_and_page_limits_fail_without_an_output() {
        let ordinary = EditSession::new(sample(), 6);
        for (scope, key) in [("catalog", "ViewerPreferences"), ("catalog", "OCProperties"), ("catalog", "AcroForm"), ("catalog", "Outlines"), ("page", "Annots"), ("page", "AA"), ("page", "StructParents"), ("tree", "UnknownInherited")] {
            let mut document = Document::load_mem(&sample()).unwrap();
            if scope == "catalog" { document.catalog_mut().unwrap().set(key, dictionary! {}); }
            else {
                let id = if scope == "page" { document.get_pages()[&1] } else { document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap() };
                document.get_dictionary_mut(id).unwrap().set(key, dictionary! {});
            }
            let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
            let unsupported = EditSession::new(bytes, 6);
            assert!(validate_sources(&ordinary, &unsupported).unwrap_err().contains(key), "{scope}/{key}");
        }
        let mut large = EditSession::new(sample(), 6); large.plan.resize(4091, large.plan[0].clone());
        assert!(validate_sources(&ordinary, &large).unwrap_err().contains("4096"));
        large.plan.clear(); assert!(validate_sources(&ordinary, &large).is_err());
    }
    #[test]
    fn combine_checks_original_tree_features_before_existing_edits_can_discard_them_and_blocks_page_links() {
        let ordinary = EditSession::new(sample(), 6);
        let mut document = Document::load_mem(&sample()).unwrap();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        document.get_dictionary_mut(root).unwrap().set("UnknownInherited", 1);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
        let mut edited = EditSession::new(bytes, 6); edited.apply(PageEdit::Move { from: 0, to: 2 }).unwrap();
        assert!(assemble(&ordinary, &edited).unwrap_err().contains("UnknownInherited"));
        let mut document = Document::load_mem(&sample()).unwrap();
        let first = document.get_pages()[&1]; let second = document.get_pages()[&2];
        let resources = inherited(&document, first, b"Resources").unwrap().unwrap();
        let mut resources = resources.as_dict().unwrap().clone(); resources.set("UnsafePageReference", second);
        document.get_dictionary_mut(first).unwrap().set("Resources", resources);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
        assert!(assemble(&ordinary, &EditSession::new(bytes, 6)).unwrap_err().contains("old catalog or page tree"));
        let mut document = Document::load_mem(&sample()).unwrap();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        document.get_dictionary_mut(root).unwrap().set("Count", 5);
        assert!(supported_shape(&document).unwrap_err().contains("counts"));
    }
    #[test]
    fn combine_signed_malformed_repeated_tree_and_exact_limits_fail_before_publishing() {
        let ordinary = EditSession::new(sample(), 6);
        let mut signed = Document::load_mem(&sample()).unwrap();
        signed.add_object(dictionary! { "Type" => "Sig", "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] });
        let mut bytes = Vec::new(); signed.save_to(&mut bytes).unwrap();
        assert!(assemble(&ordinary, &EditSession::new(bytes, 6)).unwrap_err().contains("Signed"));
        assert!(assemble(&ordinary, &EditSession::new(b"not a PDF".to_vec(), 6)).is_err());
        let mut repeated = Document::load_mem(&sample()).unwrap();
        let root = repeated.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        let page = repeated.get_pages()[&1];
        repeated.get_dictionary_mut(root).unwrap().set("Kids", vec![Object::Reference(page), Object::Reference(page)]);
        repeated.get_dictionary_mut(root).unwrap().set("Count", 2);
        assert!(supported_shape(&repeated).unwrap_err().contains("repeats"));
        let mut cycle = Document::load_mem(&sample()).unwrap();
        let root = cycle.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        cycle.get_dictionary_mut(root).unwrap().set("Kids", vec![Object::Reference(root)]);
        assert!(supported_shape(&cycle).unwrap_err().contains("cycle"));
        let mut boundary = EditSession::new(sample(), 6); boundary.plan.resize(MAX_PAGES - 6, boundary.plan[0].clone());
        assert_eq!(page_count(&ordinary, &boundary).unwrap(), MAX_PAGES);
        boundary.plan.push(boundary.plan[0].clone()); assert!(page_count(&ordinary, &boundary).is_err());
        output_size(MAX_OUTPUT_BYTES).unwrap(); assert!(output_size(MAX_OUTPUT_BYTES + 1).unwrap_err().contains("256 MiB"));
    }
    #[test]
    fn combine_validation_failure_and_existing_or_racing_files_never_publish_or_overwrite() {
        let first = EditSession::new(sample(), 6); let second = EditSession::new(sample(), 6);
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("combined.pdf");
        assert!(prepare_and_write(&first, &second, &path, |_, _| Err("Injected validation failure".into())).unwrap_err().contains("Injected"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
        let result = prepare_and_write(&first, &second, &path, |bytes, expected| { assert_eq!(Document::load_mem(bytes).unwrap().get_pages().len(), expected); std::fs::write(&path, b"racing target").unwrap(); Ok(()) });
        assert!(result.is_err()); assert_eq!(std::fs::read(&path).unwrap(), b"racing target");
        assert!(prepare_and_write(&first, &second, &path, |_, _| panic!("Existing file must reject before validation")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"racing target");
    }
}
