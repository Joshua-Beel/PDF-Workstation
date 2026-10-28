use lopdf::{dictionary, Document, Object, ObjectId};
use serde::Deserialize;
use std::{collections::{HashSet, VecDeque}, io::Write, path::Path};

const HISTORY_BUDGET: usize = 32 * 1024 * 1024;

fn snapshot_bytes(snapshot: &Vec<PageSpec>) -> usize {
    std::mem::size_of::<Vec<PageSpec>>() + snapshot.capacity() * std::mem::size_of::<PageSpec>()
        + snapshot.iter().map(|page| page.notes.capacity() * std::mem::size_of::<crate::comments::Note>()
            + page.notes.iter().map(|note| note.id.capacity() + note.contents.capacity() + note.quads.as_ref().map_or(0, |quads| quads.capacity() * std::mem::size_of::<CropBox>())).sum::<usize>()).sum::<usize>()
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageSpec {
    pub source: usize,
    pub turns: i32,
    pub crop: Option<CropBox>,
    pub notes: Vec<crate::comments::Note>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CropBox {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl CropBox {
    fn valid(self) -> bool {
        [self.left, self.bottom, self.right, self.top].iter().all(|value| value.is_finite())
            && self.right > self.left && self.top > self.bottom
    }
    fn contains(self, other: Self) -> bool {
        other.left >= self.left && other.bottom >= self.bottom && other.right <= self.right && other.top <= self.top
    }
    pub fn validate_within(self, visible: Self) -> Result<(), String> {
        if !self.valid() || self.right - self.left < 1.0 || self.top - self.bottom < 1.0 {
            return Err("Keep at least one PDF point of width and height when cropping.".into());
        }
        if !visible.valid() || !visible.contains(self) { return Err("The crop must stay within the page's current visible area.".into()); }
        Ok(())
    }
    fn object(self) -> Object {
        Object::Array([self.left, self.bottom, self.right, self.top].into_iter().map(Object::Real).collect())
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageEdit {
    Rotate { pages: Vec<usize>, clockwise: bool },
    Delete { pages: Vec<usize> },
    Move { from: usize, to: usize },
    #[serde(skip_deserializing)]
    Crop { page: usize, crop: CropBox },
    #[serde(skip_deserializing)]
    CropMany { crops: Vec<(usize, CropBox)> },
    #[serde(skip_deserializing)]
    ResetCropMany { pages: Vec<usize> },
    Undo,
    Redo,
}

pub(crate) fn source_text_extraction_permission(document: &Document) -> Result<(), String> {
    let permissions = if let Some(state) = document.encryption_state.as_ref() {
        state.permissions()
    } else if document.is_encrypted() {
        let bits = document.get_encrypted()
            .and_then(|dictionary| dictionary.get(b"P"))
            .and_then(Object::as_i64)
            .map_err(|_| "The PDF extraction permission could not be verified.")?;
        lopdf::Permissions::from_bits_truncate(bits as u64)
    } else {
        return Ok(());
    };
    if permissions.contains(lopdf::Permissions::COPYABLE) {
        Ok(())
    } else {
        Err("This PDF does not allow text and graphics extraction.".into())
    }
}

/// A document's immutable source bytes. Cloning shares the one buffer, so the in-process PDFium document can
/// read the same allocation the session holds instead of keeping a second copy of every open PDF.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceBytes(std::sync::Arc<Vec<u8>>);
impl From<Vec<u8>> for SourceBytes { fn from(bytes: Vec<u8>) -> Self { Self(std::sync::Arc::new(bytes)) } }
impl std::ops::Deref for SourceBytes { type Target = [u8]; fn deref(&self) -> &[u8] { &self.0 } }
impl AsRef<[u8]> for SourceBytes { fn as_ref(&self) -> &[u8] { &self.0 } }
impl std::fmt::Debug for SourceBytes { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("SourceBytes").field("len", &self.0.len()).finish() } }
impl PartialEq<Vec<u8>> for SourceBytes { fn eq(&self, other: &Vec<u8>) -> bool { self.0.as_slice() == other.as_slice() } }
impl PartialEq<SourceBytes> for Vec<u8> { fn eq(&self, other: &SourceBytes) -> bool { self.as_slice() == other.0.as_slice() } }
impl PartialEq<[u8]> for SourceBytes { fn eq(&self, other: &[u8]) -> bool { self.0.as_slice() == other } }
impl PartialEq<&[u8]> for SourceBytes { fn eq(&self, other: &&[u8]) -> bool { self.0.as_slice() == *other } }
impl<const N: usize> PartialEq<[u8; N]> for SourceBytes { fn eq(&self, other: &[u8; N]) -> bool { self.0.as_slice() == other.as_slice() } }
impl<const N: usize> PartialEq<&[u8; N]> for SourceBytes { fn eq(&self, other: &&[u8; N]) -> bool { self.0.as_slice() == other.as_slice() } }

pub struct EditSession {
    pub source: SourceBytes,
    source_page_count: usize,
    pub plan: Vec<PageSpec>,
    undo: VecDeque<Vec<PageSpec>>,
    redo: VecDeque<Vec<PageSpec>>,
    history_bytes: usize,
    history_budget: usize,
    saved: Vec<PageSpec>,
    pub revision: u64,
    source_notes: Vec<Vec<crate::comments::Note>>,
    comments_reason: Option<String>,
    text_extraction_permission: Result<(), String>,
    next_note: u64,
    pub outline: crate::outline_edit::OutlineEdits,
    pub labels: crate::page_label_edit::LabelEdits,
}

pub(crate) struct EditSessionSnapshot {
    plan: Vec<PageSpec>, undo: VecDeque<Vec<PageSpec>>, redo: VecDeque<Vec<PageSpec>>,
    history_bytes: usize, saved: Vec<PageSpec>, revision: u64, next_note: u64,
}

impl EditSession {
    pub(crate) fn snapshot(&self) -> EditSessionSnapshot {
        EditSessionSnapshot { plan: self.plan.clone(), undo: self.undo.clone(), redo: self.redo.clone(), history_bytes: self.history_bytes, saved: self.saved.clone(), revision: self.revision, next_note: self.next_note }
    }
    pub(crate) fn restore(&mut self, snapshot: EditSessionSnapshot) {
        self.plan = snapshot.plan; self.undo = snapshot.undo; self.redo = snapshot.redo;
        self.history_bytes = snapshot.history_bytes; self.saved = snapshot.saved;
        self.revision = snapshot.revision; self.next_note = snapshot.next_note;
    }
    pub fn new(source: impl Into<SourceBytes>, count: usize) -> Self {
        let source = source.into();
        let parsed = crate::pdf_load::load(&source).map_err(|error| error.to_string());
        Self::opened(source, parsed, count, None)
    }
    /// A session for a source already parsed by `pdf_load::load_for_open`, so an open parses it once.
    pub(crate) fn opened(source: impl Into<SourceBytes>, parsed: Result<Document, String>, count: usize, password: Option<&str>) -> Self {
        let source = source.into();
        let owner_access = parsed.as_ref().ok().is_some_and(|document| password.is_some_and(|password| document.authenticate_owner_password(password).is_ok()));
        let text_extraction_permission = if owner_access {
            Ok(())
        } else if parsed.as_ref().is_err_and(|error| error.as_str() == crate::xref_resolve::AMBIGUOUS) {
            crate::xref_resolve::refused_source_permission(&source)
        } else {
            parsed.as_ref().map_err(|_| "The PDF extraction permission could not be verified.".into()).and_then(|document| source_text_extraction_permission(document))
        };
        let imported = parsed.and_then(|document| crate::comments::read(&document));
        let (source_notes, comments_reason) = match imported {
            Ok(notes) if notes.len() == count => (notes, None),
            Ok(_) => (vec![Vec::new(); count], Some("The PDF engines disagree about the page count.".into())),
            Err(reason) => (vec![Vec::new(); count], Some(reason)),
        };
        let next_note = source_notes.iter().flatten().filter_map(|note| crate::comments::number(&note.id).ok()).max().unwrap_or(0).saturating_add(1);
        let plan: Vec<_> = (0..count).map(|source| PageSpec { source, turns: 0, crop: None, notes: source_notes[source].clone() }).collect();
        Self { source, source_page_count: count, saved: plan.clone(), plan, undo: VecDeque::new(), redo: VecDeque::new(), history_bytes: 0, history_budget: HISTORY_BUDGET, revision: 0, source_notes, comments_reason, text_extraction_permission, next_note, outline: Default::default(), labels: Default::default() }
    }
    pub fn dirty(&self) -> bool { self.plan != self.saved }
    /// Unsaved page edits, bookmark edits or page-label edits. The last two are not part of the page plan or of recovery points.
    pub fn unsaved(&self) -> bool { self.dirty() || self.outline.changed() || self.labels.changed() }
    pub fn overlay_pending(&self) -> bool { self.outline.changed() || self.labels.changed() }
    /// Copies built from the source bytes cannot carry bookmark or page-label overlay edits, so they refuse while any are pending.
    pub fn overlay_guard(&self) -> Result<(), String> {
        if self.overlay_pending() { Err("Save a copy with your bookmark and page-label edits first, or discard them.".into()) } else { Ok(()) }
    }
    pub fn discard_outline_and_labels(&mut self) { self.outline.discard(); self.labels.discard(); }
    fn load_labels(&mut self) {
        if self.labels.loaded() { return; }
        let count = self.source_page_count;
        let result = self.load_source().and_then(|document| { check_supported(&document, false, false)?; crate::page_labels::read_ranges(&document, count) });
        self.labels.load(result, count);
    }
    pub fn label_view(&mut self) -> crate::page_label_edit::EditView { self.load_labels(); self.labels.view(&self.plan) }
    pub fn label_edit(&mut self, version: u64, op: crate::page_label_edit::LabelOp) -> Result<crate::page_label_edit::EditView, String> {
        self.load_labels();
        self.labels.apply(version, op, &self.plan)?;
        Ok(self.labels.view(&self.plan))
    }
    fn load_outline(&mut self) {
        if self.outline.loaded() { return; }
        let result = self.load_source().and_then(|document| { check_supported(&document, false, false)?; crate::outline_edit::read(&document) });
        self.outline.load(result);
    }
    pub fn bookmark_view(&mut self) -> crate::outline_edit::EditView { self.load_outline(); self.outline.view(&self.plan) }
    pub fn bookmark_edit(&mut self, version: u64, op: crate::outline_edit::BookmarkOp) -> Result<crate::outline_edit::EditView, String> {
        self.load_outline();
        self.outline.apply(version, op, &self.plan)?;
        Ok(self.outline.view(&self.plan))
    }
    pub fn can_undo(&self) -> bool { !self.undo.is_empty() }
    pub fn can_redo(&self) -> bool { !self.redo.is_empty() }
    pub fn mark_saved(&mut self) { self.saved = self.plan.clone(); }
    pub fn comments_reason(&self) -> Option<&str> { self.comments_reason.as_deref() }
    pub fn text_extraction_permission(&self) -> Result<(), String> { self.text_extraction_permission.clone() }
    pub fn page_image_export_guard(&self) -> Result<(), String> {
        let document = self.load_source()?;
        check_supported(&document, false, false)
    }
    pub fn proposed_comment(&self, page: Option<(usize, CropBox)>, note_id: Option<&str>, contents: Option<&str>) -> Result<Vec<PageSpec>, String> {
        if let Some(reason) = &self.comments_reason { return Err(reason.clone()); }
        let document = self.load_source()?; crate::comments::read(&document)?;
        let mut next = self.plan.clone();
        match (page, note_id, contents) {
            (Some((page, rect)), None, Some(contents)) => {
                crate::comments::validate_text(contents)?;
                if self.next_note == u64::MAX { return Err("This document has exhausted its note IDs.".into()); }
                let spec = next.get_mut(page).ok_or("Page is out of range")?;
                let page_id = *document.get_pages().values().nth(spec.source).ok_or("Source page mapping is invalid.")?;
                rect.validate_within(spec.crop.unwrap_or(visible_box(&document, page_id)?))?;
                spec.notes.push(crate::comments::Note { quads: None, id: crate::comments::id(self.next_note), rect, contents: contents.to_owned(), kind: crate::comments::AnnotationKind::Note });
            }
            (None, Some(id), contents) => {
                let (page, position) = next.iter().enumerate().find_map(|(page, spec)| spec.notes.iter().position(|note| note.id == id).map(|position| (page, position))).ok_or("The note no longer exists. Refresh comments.")?;
                if next[page].notes[position].kind != crate::comments::AnnotationKind::Note { return Err("Choose the Area highlight editor for this annotation.".into()); }
                match contents { Some(contents) => { crate::comments::validate_text(contents)?; next[page].notes[position].contents = contents.to_owned(); }, None => { next[page].notes.remove(position); } }
            }
            _ => return Err("Invalid comment operation.".into()),
        }
        crate::comments::validate_plan(&next)?; Ok(next)
    }
    pub fn proposed_highlight(&self, page: Option<(usize, CropBox)>, annotation_id: Option<&str>, contents: Option<&str>, delete: bool) -> Result<Vec<PageSpec>, String> {
        if let Some(reason) = &self.comments_reason { return Err(reason.clone()); }
        let document = self.load_source()?; crate::comments::read(&document)?;
        let mut next = self.plan.clone();
        match (page, annotation_id, delete) {
            (Some((page, rect)), None, false) => {
                let contents = crate::comments::highlight_contents(contents)?;
                if self.next_note == u64::MAX { return Err("This document has exhausted its annotation IDs.".into()); }
                let spec = next.get_mut(page).ok_or("Page is out of range")?;
                let page_id = *document.get_pages().values().nth(spec.source).ok_or("Source page mapping is invalid.")?;
                rect.validate_within(spec.crop.unwrap_or(visible_box(&document, page_id)?))?;
                spec.notes.push(crate::comments::Note { quads: None, id: crate::comments::highlight_id(self.next_note), rect, contents, kind: crate::comments::AnnotationKind::Highlight });
            }
            (None, Some(id), delete) => {
                let (page, position) = next.iter().enumerate().find_map(|(page, spec)| spec.notes.iter().position(|note| note.id == id).map(|position| (page, position))).ok_or("The highlight no longer exists. Refresh annotations.")?;
                if next[page].notes[position].kind != crate::comments::AnnotationKind::Highlight { return Err("Choose the Comment editor for this annotation.".into()); }
                if delete { next[page].notes.remove(position); } else { next[page].notes[position].contents = crate::comments::highlight_contents(contents)?; }
            }
            _ => return Err("Invalid Area highlight operation.".into()),
        }
        crate::comments::validate_plan(&next)?; Ok(next)
    }
    pub fn proposed_text_highlight(&self, page: usize, quads: Vec<CropBox>, contents: Option<&str>) -> Result<Vec<PageSpec>, String> {
        if let Some(reason) = &self.comments_reason { return Err(reason.clone()); }
        let document = self.load_source()?; crate::comments::read(&document)?;
        if self.next_note == u64::MAX { return Err("This document has exhausted its annotation IDs.".into()); }
        let bounds = crate::comments::highlight_union(&quads)?;
        let mut next = self.plan.clone();
        let spec = next.get_mut(page).ok_or("Page is out of range")?;
        let page_id = *document.get_pages().values().nth(spec.source).ok_or("Source page mapping is invalid.")?;
        let visible = spec.crop.unwrap_or(visible_box(&document, page_id)?);
        if bounds.left < visible.left || bounds.bottom < visible.bottom || bounds.right > visible.right || bounds.top > visible.top { return Err("The selected text is outside the current visible page.".into()); }
        spec.notes.push(crate::comments::Note { id: crate::comments::highlight_id(self.next_note), rect: bounds, contents: crate::comments::highlight_contents(contents)?, kind: crate::comments::AnnotationKind::Highlight, quads: Some(quads) });
        crate::comments::validate_plan(&next)?; Ok(next)
    }
    fn notes_changed(&self, plan: &[PageSpec]) -> bool {
        plan.iter().any(|spec| self.source_notes.get(spec.source) != Some(&spec.notes))
    }
    pub fn note_source(&self, plan: &[PageSpec]) -> Result<Option<Vec<u8>>, String> {
        if !self.notes_changed(plan) { return Ok(None); }
        let mut document = self.load_source()?; crate::comments::write(&mut document, plan)?;
        crate::pdf_load::prune_unreachable(&mut document);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).map_err(|error| error.to_string())?; Ok(Some(bytes))
    }
    pub fn commit_comments(&mut self, next: Vec<PageSpec>) {
        if next == self.plan { return; }
        self.next_note = self.next_note.max(next.iter().flat_map(|page| &page.notes).filter_map(|note| crate::comments::number(&note.id).ok()).max().unwrap_or(0).saturating_add(1));
        self.commit_plan(next);
    }
    fn commit_plan(&mut self, next: Vec<PageSpec>) {
        let previous = std::mem::replace(&mut self.plan, next);
        self.history_bytes += snapshot_bytes(&previous); self.undo.push_back(previous);
        self.history_bytes -= self.redo.drain(..).map(|snapshot| snapshot_bytes(&snapshot)).sum::<usize>();
        self.trim_history(); self.revision += 1;
    }
    fn trim_history(&mut self) {
        while self.history_bytes > self.history_budget {
            let snapshot = self.undo.pop_front().or_else(|| self.redo.pop_front()).expect("History byte count matches stored snapshots");
            self.history_bytes -= snapshot_bytes(&snapshot);
        }
    }
    fn load_source(&self) -> Result<Document, String> {
        let document = crate::pdf_load::load(&self.source).map_err(|e| e.to_string())?;
        if document.is_encrypted() || document.encryption_state.is_some() { return Err("Encrypted PDFs cannot be edited in this build.".into()); }
        let pages = document.get_pages();
        if pages.len() != self.source_page_count {
            return Err("The PDF engines disagree about the source page count. Editing and export are blocked to preserve the document.".into());
        }
        if pages.values().collect::<HashSet<_>>().len() != pages.len() {
            return Err("The PDF repeats a page object in its page tree. Editing and export are blocked to avoid changing other pages.".into());
        }
        Ok(document)
    }

    pub fn apply(&mut self, edit: PageEdit) -> Result<(), String> {
        match edit {
            PageEdit::Undo => {
                if self.labels.changed() && !self.undo.back().is_some_and(|previous| crate::page_label_edit::plan_in_source_order(previous, self.source_page_count)) { return Err(crate::page_label_edit::PENDING_REFUSAL.into()); }
                let previous = self.undo.pop_back().ok_or("Nothing to undo")?;
                self.history_bytes -= snapshot_bytes(&previous);
                let current = std::mem::replace(&mut self.plan, previous);
                self.history_bytes += snapshot_bytes(&current);
                self.redo.push_back(current);
            }
            PageEdit::Redo => {
                if self.labels.changed() && !self.redo.back().is_some_and(|next| crate::page_label_edit::plan_in_source_order(next, self.source_page_count)) { return Err(crate::page_label_edit::PENDING_REFUSAL.into()); }
                let next = self.redo.pop_back().ok_or("Nothing to redo")?;
                self.history_bytes -= snapshot_bytes(&next);
                let current = std::mem::replace(&mut self.plan, next);
                self.history_bytes += snapshot_bytes(&current);
                self.undo.push_back(current);
            }
            edit => {
                let mut next = self.plan.clone();
                let structural = !matches!(&edit, PageEdit::Rotate { .. } | PageEdit::Crop { .. } | PageEdit::CropMany { .. } | PageEdit::ResetCropMany { .. });
                let removal = matches!(&edit, PageEdit::Delete { .. });
                if structural && self.labels.changed() { return Err(crate::page_label_edit::PENDING_REFUSAL.into()); }
                let document = self.load_source()?;
                check_supported(&document, structural, removal)?;
                match edit {
                    PageEdit::Rotate { pages, clockwise } => {
                        for index in validate_selection(&pages, next.len())? {
                            next[index].turns = (next[index].turns + if clockwise { 1 } else { 3 }) % 4;
                        }
                    }
                    PageEdit::Delete { pages } => {
                        let selected = validate_selection(&pages, next.len())?;
                        if selected.len() == next.len() { return Err("Keep at least one page in the document.".into()); }
                        next = next.into_iter().enumerate().filter_map(|(i, page)| (!selected.contains(&i)).then_some(page)).collect();
                    }
                    PageEdit::Move { from, to } => {
                        if from >= next.len() || to >= next.len() { return Err("Page position is out of range.".into()); }
                        let page = next.remove(from); next.insert(to, page);
                    }
                    PageEdit::Crop { page, crop } => {
                        let spec = next.get_mut(page).ok_or("Page is out of range")?;
                        let visible = validate_crop(&document, spec, crop)?;
                        if crop == visible { return Ok(()); }
                        spec.crop = Some(crop);
                    }
                    PageEdit::CropMany { crops } => {
                        if crops.is_empty() || crops.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
                            return Err("Select one or more pages in ascending order without duplicates.".into());
                        }
                        for &(page, crop) in &crops {
                            let spec = next.get(page).ok_or("Page is out of range")?;
                            if validate_crop(&document, spec, crop)? == crop {
                                return Err("The insets are too small to change every selected page.".into());
                            }
                        }
                        for (page, crop) in crops { next[page].crop = Some(crop); }
                    }
                    PageEdit::ResetCropMany { pages } => {
                        if pages.is_empty() || pages.windows(2).any(|pair| pair[0] >= pair[1]) || pages.iter().any(|&page| page >= next.len()) {
                            return Err("Select one or more pages in ascending order without duplicates.".into());
                        }
                        for &page in &pages {
                            let spec = &next[page];
                            let visible = source_visible_box(&document, spec)?;
                            if let Some(crop) = spec.crop { crop.validate_within(visible)?; }
                        }
                        for page in pages { next[page].crop = None; }
                    }
                    _ => unreachable!(),
                }
                if next == self.plan { return Ok(()); }
                self.commit_plan(next);
                return Ok(());
            }
        }
        self.trim_history();
        self.revision += 1;
        Ok(())
    }

    pub fn export(&self, selection: Option<&[usize]>) -> Result<Vec<u8>, String> {
        let plan = match selection {
            Some(selection) => {
                let selected = validate_selection(selection, self.plan.len())?;
                self.plan.iter().enumerate().filter_map(|(i, p)| selected.contains(&i).then_some(p.clone())).collect::<Vec<_>>()
            }
            None => self.plan.clone(),
        };
        let mut document = self.load_source()?;
        if self.notes_changed(&self.plan) { crate::comments::write(&mut document, &self.plan)?; }
        let original: Vec<_> = document.get_pages().values().copied().collect();
        if plan.iter().any(|p| p.source >= original.len()) { return Err("Source page mapping is invalid.".into()); }
        let structural = plan.len() != original.len() || plan.iter().enumerate().any(|(i, p)| p.source != i);
        check_supported(&document, structural, plan.len() < original.len())?;
        if let Some(entries) = self.outline.pending() {
            let kept: HashSet<usize> = plan.iter().map(|spec| spec.source).collect();
            crate::outline_edit::write(&mut document, entries, &|source| kept.contains(&source).then(|| original.get(source).copied()).flatten())?;
        }
        if let Some(ranges) = self.labels.pending() {
            let output = crate::page_label_edit::output_ranges(ranges, original.len(), &plan)?;
            crate::page_label_edit::write(&mut document, &output)?;
        }
        let new_root = structural.then(|| document.new_object_id());
        let mut kids = Vec::new();
        for spec in &plan {
            let id = original[spec.source];
            let rotation = inherited(&document, id, b"Rotate")?.map(|o| o.as_i64().map_err(|e| e.to_string())).transpose()?.unwrap_or(0);
            if rotation % 90 != 0 { return Err("This document has an unsupported page rotation.".into()); }
            if let Some(crop) = spec.crop { crop.validate_within(visible_box(&document, id)?)?; }
            let attributes: Vec<_> = if structural {
                [b"Resources".as_slice(), b"MediaBox", b"CropBox"].iter().map(|key| Ok((key.to_vec(), inherited(&document, id, key)?))).collect::<Result<_, String>>()?
            } else { Vec::new() };
            let page = document.get_object_mut(id).map_err(|e| e.to_string())?.as_dict_mut().map_err(|e| e.to_string())?;
            page.set("Rotate", (rotation.rem_euclid(360) + i64::from(spec.turns) * 90).rem_euclid(360));
            if let Some(root) = new_root {
                for (key, value) in attributes { if let Some(value) = value { page.set(key, value); } }
                page.set("Parent", root);
                kids.push(Object::Reference(id));
            }
            if let Some(crop) = spec.crop { page.set("CropBox", crop.object()); }
        }
        if let Some(root) = new_root {
            document.objects.insert(root, Object::Dictionary(dictionary! { "Type" => "Pages", "Count" => plan.len() as i64, "Kids" => kids }));
            document.catalog_mut().map_err(|e| e.to_string())?.set("Pages", root);
            // A removed page, or a node of the old page tree (whose Kids list every source page), can
            // still be referenced from a custom key on a kept page, the catalog or the trailer; null
            // those references so pruning does not keep removed pages alive.
            let kept: HashSet<ObjectId> = plan.iter().map(|spec| original[spec.source]).collect();
            let old_tree = document.objects.iter().filter(|(id, object)| **id != root && object.as_dict().ok().and_then(|dict| dict.get(b"Type").ok()).and_then(|value| value.as_name().ok()) == Some(b"Pages".as_slice())).map(|(id, _)| *id);
            let removed: HashSet<ObjectId> = original.iter().copied().filter(|id| !kept.contains(id)).chain(old_tree).collect();
            remove_page_references(&mut document, removed);
        }
        // Keep only objects reachable from the trailer (SM7 L-1): superseded incremental revisions and
        // freed objects reloaded from an object stream never reach a copy, structural edit or not.
        crate::pdf_load::prune_unreachable(&mut document);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

/// Cuts every reference to the removed pages (and old page-tree nodes) in `removed`. A `/Dest` or a GoTo
/// `/A` that targets one is dropped rather than left as `[null ...]`, whether the destination array is
/// inline or an indirect object; a link annotation left with neither because of that is dropped too,
/// while a link that never had a `/Dest` or `/A` is kept untouched. Nulls left in `/Annots` arrays are
/// removed. Every other reference becomes null, so pruning cannot keep a removed page alive.
pub(crate) fn remove_page_references(document: &mut Document, mut removed: HashSet<ObjectId>) {
    if removed.is_empty() { return; }
    let first_removed = |array: &Object| matches!(array.as_array().ok().and_then(|array| array.first()), Some(Object::Reference(id)) if removed.contains(id));
    let dead_arrays: HashSet<ObjectId> = document.objects.iter().filter(|(_, object)| first_removed(object)).map(|(id, _)| *id).collect();
    let targets_removed = |destination: &Object| match destination { Object::Reference(id) => dead_arrays.contains(id), other => first_removed(other) };
    let goto_removed = |action: &lopdf::Dictionary| action.get(b"S").and_then(Object::as_name).is_ok_and(|name| name == b"GoTo") && action.get(b"D").is_ok_and(targets_removed);
    let dead_actions: HashSet<ObjectId> = document.objects.iter().filter(|(_, object)| object.as_dict().is_ok_and(goto_removed)).map(|(id, _)| *id).collect();
    let mut dead_links = Vec::new();
    for (id, object) in document.objects.iter_mut() {
        let Ok(dict) = object.as_dict_mut() else { continue };
        let mut dropped = false;
        if dict.get(b"Dest").is_ok_and(targets_removed) { dict.remove(b"Dest"); dropped = true; }
        let dead_action = match dict.get(b"A") { Ok(Object::Dictionary(action)) => goto_removed(action), Ok(Object::Reference(action)) => dead_actions.contains(action), _ => false };
        if dead_action { dict.remove(b"A"); dropped = true; }
        if dropped && dict.get(b"Subtype").and_then(Object::as_name).is_ok_and(|name| name == b"Link") && !dict.has(b"Dest") && !dict.has(b"A") { dead_links.push(*id); }
    }
    removed.extend(dead_links);
    null_references(document, &removed);
    for object in document.objects.values_mut() {
        if let Ok(dict) = object.as_dict_mut() { if let Ok(Object::Array(annots)) = dict.get_mut(b"Annots") { annots.retain(|value| !matches!(value, Object::Null)); } }
    }
}

/// Replaces every reference to an object in `removed`, anywhere in the document or trailer, with null.
pub(crate) fn null_references(document: &mut Document, removed: &HashSet<ObjectId>) {
    fn scrub(value: &mut Object, removed: &HashSet<ObjectId>) {
        match value {
            Object::Reference(id) if removed.contains(id) => *value = Object::Null,
            Object::Array(values) => for value in values { scrub(value, removed); },
            Object::Dictionary(dict) => for (_, value) in dict.iter_mut() { scrub(value, removed); },
            Object::Stream(stream) => for (_, value) in stream.dict.iter_mut() { scrub(value, removed); },
            _ => {}
        }
    }
    if removed.is_empty() { return; }
    for value in document.objects.values_mut() { scrub(value, removed); }
    for (_, value) in document.trailer.iter_mut() { scrub(value, removed); }
}

fn visible_box(document: &Document, id: ObjectId) -> Result<CropBox, String> {
    let read_box = |value: Object| -> Result<CropBox, String> {
        let array = value.as_array().map_err(|_| "The page has an unsupported boundary box.")?;
        if array.len() != 4 { return Err("The page has an unsupported boundary box.".into()); }
        let values = array.iter().map(|value| document.dereference(value).map_err(|error| error.to_string())?.1.as_float().map_err(|error| error.to_string())).collect::<Result<Vec<_>, _>>()?;
        let bounds = CropBox { left: values[0], bottom: values[1], right: values[2], top: values[3] };
        if !bounds.valid() { return Err("The page has an invalid boundary box.".into()); }
        Ok(bounds)
    };
    let media = read_box(inherited(document, id, b"MediaBox")?.ok_or("The page has no supported MediaBox.")?)?;
    let crop = inherited(document, id, b"CropBox")?.map(read_box).transpose()?.unwrap_or(media);
    let visible = CropBox { left: media.left.max(crop.left), bottom: media.bottom.max(crop.bottom), right: media.right.min(crop.right), top: media.top.min(crop.top) };
    if !visible.valid() { return Err("The page has an empty visible boundary box.".into()); }
    Ok(visible)
}

fn validate_crop(document: &Document, spec: &PageSpec, crop: CropBox) -> Result<CropBox, String> {
    let source_visible = source_visible_box(document, spec)?;
    let visible = spec.crop.unwrap_or(source_visible);
    crop.validate_within(visible)?;
    Ok(visible)
}

fn source_visible_box(document: &Document, spec: &PageSpec) -> Result<CropBox, String> {
    let id = *document.get_pages().values().nth(spec.source).ok_or("Source page mapping is invalid.")?;
    let rotation = inherited(document, id, b"Rotate")?.map(|value| value.as_i64().map_err(|error| error.to_string())).transpose()?.unwrap_or(0);
    if rotation % 90 != 0 { return Err("This document has an unsupported page rotation.".into()); }
    visible_box(document, id)
}

fn validate_selection(pages: &[usize], count: usize) -> Result<HashSet<usize>, String> {
    if pages.is_empty() || pages.iter().any(|&i| i >= count) { return Err("Select valid pages first.".into()); }
    Ok(pages.iter().copied().collect())
}

fn inherited(document: &Document, mut id: ObjectId, key: &[u8]) -> Result<Option<Object>, String> {
    let mut visited = HashSet::new();
    loop {
        if !visited.insert(id) { return Err("The page tree contains a cycle.".into()); }
        let dictionary = document.get_dictionary(id).map_err(|e| e.to_string())?;
        if let Ok(value) = dictionary.get(key) {
            return Ok(Some(document.dereference(value).map_err(|e| e.to_string())?.1.clone()));
        }
        match dictionary.get(b"Parent") {
            Ok(parent) => id = parent.as_reference().map_err(|e| e.to_string())?,
            Err(_) => return Ok(None),
        }
    }
}

fn check_supported(document: &Document, structural: bool, removal: bool) -> Result<(), String> {
    if document.is_encrypted() { return Err("Encrypted PDFs cannot be edited in this build.".into()); }
    let catalog = document.catalog().map_err(|e| e.to_string())?;
    if catalog.has(b"Perms") || document.objects.values().any(|object| object.as_dict().is_ok_and(|dict| dict.has(b"ByteRange") || dict.get(b"Type").is_ok_and(|value| value.as_name().is_ok_and(|name| name == b"Sig")))) {
        return Err("Signed or certified PDFs cannot be edited in this build; signatures must remain intact.".into());
    }
    if structural && [b"AcroForm".as_slice(), b"StructTreeRoot", b"PageLabels", b"Threads"].iter().any(|key| catalog.has(key)) {
        return Err("Reordering/extraction/deletion of forms, tagged PDFs, page labels, or article threads is not supported yet. Rotation is available.".into());
    }
    if removal && ([b"Outlines".as_slice(), b"Dests", b"Names", b"OpenAction"].iter().any(|key| catalog.has(key)) || (document.get_pages().values().any(|id| document.get_dictionary(*id).is_ok_and(|page| page.has(b"Annots"))) && crate::comments::read(document).is_err())) {
        return Err("Deleting/extracting pages with bookmarks, destinations, attachments, actions, or annotations is not supported yet. This avoids losing or breaking their references.".into());
    }
    Ok(())
}

pub fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path.exists() { return Err("That file already exists. Choose a new filename; Save a Copy never overwrites an existing file.".into()); }
    if !path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) { return Err("The output filename must end in .pdf.".into()); }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).ok_or("Choose an output folder.")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(bytes).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist_noclobber(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Vec<u8> { std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/welcome.pdf")).unwrap() }
    #[test]
    fn text_highlights_history_counts_quad_capacity_and_keeps_saved_plan() {
        let mut session = EditSession::new(sample(), 6); let bounds = CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 };
        let next = session.proposed_text_highlight(0, vec![bounds; 256], Some("Saved text highlight")).unwrap(); session.commit_comments(next); session.mark_saved();
        let saved = session.saved.clone(); let id = session.plan[0].notes[0].id.clone(); let allocated = snapshot_bytes(&session.plan);
        let mut measured = session.plan.clone(); measured[0].notes[0].quads.as_mut().unwrap().reserve(256); let with_quads = snapshot_bytes(&measured); let quads = measured[0].notes[0].quads.take().unwrap();
        assert!(quads.capacity() > quads.len()); assert_eq!(with_quads - snapshot_bytes(&measured), quads.capacity() * std::mem::size_of::<CropBox>(), "History must count allocated quad capacity, including spare slots, independently of unrelated clone capacities");
        session.history_budget = allocated * 2;
        for index in 0..6 { let next = session.proposed_highlight(None, Some(&id), Some(&format!("Body {index}")), false).unwrap(); session.commit_comments(next); assert_history_budget(&session); }
        assert_eq!(session.saved, saved); assert_eq!(session.plan[0].notes[0].quads, saved[0].notes[0].quads); session.apply(PageEdit::Undo).unwrap(); session.apply(PageEdit::Redo).unwrap(); assert_history_budget(&session);
    }
    #[test]
    fn comments_history_counts_string_capacity_and_evicts_without_losing_current_or_saved_notes() {
        let mut session = EditSession::new(sample(), 6);
        let bounds = CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 };
        let plan = session.proposed_comment(Some((0, bounds)), None, Some(&"x".repeat(8192))).unwrap(); session.commit_comments(plan); session.mark_saved();
        let plan = session.proposed_highlight(Some((0, bounds)), None, Some(&"z".repeat(8192)), false).unwrap(); session.commit_comments(plan); session.mark_saved();
        let saved = session.saved.clone(); let source = session.source.clone(); let id = session.plan[0].notes[0].id.clone();
        assert!(snapshot_bytes(&session.plan) >= 16384 + snapshot_bytes(&vec![PageSpec { source: 0, turns: 0, crop: None, notes: Vec::new() }]));
        session.history_budget = snapshot_bytes(&session.plan) * 2;
        for value in 0..6 { let plan = session.proposed_comment(None, Some(&id), Some(&format!("{value}{}", "y".repeat(8191)))).unwrap(); session.commit_comments(plan); assert_history_budget(&session); }
        assert_eq!(session.saved, saved); assert_eq!(session.source, source); assert_eq!(session.plan[0].notes[0].contents.len(), 8192);
        session.apply(PageEdit::Undo).unwrap(); assert_history_budget(&session); session.apply(PageEdit::Redo).unwrap(); assert_history_budget(&session);
        let plan = session.proposed_comment(None, Some(&id), Some("branch")).unwrap(); session.commit_comments(plan); assert!(!session.can_redo()); assert_history_budget(&session);
    }
    fn assert_history_budget(session: &EditSession) {
        let allocated = session.undo.iter().chain(session.redo.iter()).map(snapshot_bytes).sum::<usize>();
        assert_eq!(session.history_bytes, allocated);
        assert!(allocated <= session.history_budget);
    }
    #[test]
    fn crop_plan_and_export_preserve_content_references_source_and_saved_history() {
        let mut document = Document::load_mem(&sample()).unwrap();
        let first = document.get_pages()[&1];
        let annotation = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![20.into(), 20.into(), 40.into(), 40.into()] });
        document.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("Annots", vec![Object::Reference(annotation)]);
        for key in ["AcroForm", "StructTreeRoot", "PageLabels", "Threads", "Outlines", "Names"] { document.catalog_mut().unwrap().set(key, dictionary! {}); }
        document.catalog_mut().unwrap().set("OpenAction", vec![Object::Reference(first), Object::Name(b"Fit".to_vec())]);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
        let mut session = EditSession::new(bytes.clone(), 6);
        let full = visible_box(&document, first).unwrap();
        session.apply(PageEdit::Crop { page: 0, crop: full }).unwrap();
        assert_eq!(session.revision, 0); assert!(!session.can_undo()); assert!(!session.dirty());
        let crop = CropBox { left: 20.0, bottom: 40.0, right: 590.0, top: 750.0 };
        session.apply(PageEdit::Crop { page: 0, crop }).unwrap();
        assert_eq!(session.plan[0].crop, Some(crop)); assert_eq!(session.revision, 1);
        let undo_bytes = session.history_bytes;
        session.apply(PageEdit::Crop { page: 0, crop }).unwrap();
        assert_eq!(session.revision, 1); assert_eq!(session.history_bytes, undo_bytes);
        session.mark_saved();
        let smaller = CropBox { left: 30.0, bottom: 50.0, right: 580.0, top: 740.0 };
        session.apply(PageEdit::Crop { page: 0, crop: smaller }).unwrap();
        assert!(session.apply(PageEdit::Crop { page: 0, crop }).unwrap_err().contains("current visible area"));
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        assert_eq!(document.get_pages(), output.get_pages());
        assert_eq!(output.get_page_content(first), document.get_page_content(first));
        assert_eq!(visible_box(&output, first).unwrap(), smaller);
        assert_eq!(output.catalog().unwrap(), document.catalog().unwrap());
        assert_eq!(output.get_object(annotation).unwrap(), document.get_object(annotation).unwrap());
        session.apply(PageEdit::Undo).unwrap(); assert!(!session.dirty()); assert_eq!(session.plan[0].crop, Some(crop));
        session.apply(PageEdit::Redo).unwrap(); assert!(session.dirty()); assert_eq!(session.plan[0].crop, Some(smaller));
        session.apply(PageEdit::Undo).unwrap();
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        assert!(!session.can_redo()); assert_eq!(session.plan[0].crop, Some(crop));
        assert_eq!(session.source, bytes); assert_history_budget(&session);
        assert!(serde_json::from_str::<PageEdit>(r#"{"kind":"crop","page":0,"crop":{"left":0,"bottom":0,"right":1,"top":1}}"#).is_err(), "Source-coordinate crop cannot bypass the dedicated revision-checked IPC");
    }
    #[test]
    fn batch_crop_validates_every_target_before_one_history_commit() {
        let source = sample();
        let document = Document::load_mem(&source).unwrap();
        let pages: Vec<_> = document.get_pages().values().copied().collect();
        let first = visible_box(&document, pages[0]).unwrap();
        let third = visible_box(&document, pages[2]).unwrap();
        let first_crop = CropBox { left: first.left + 10.0, bottom: first.bottom + 20.0, right: first.right - 30.0, top: first.top - 40.0 };
        let third_crop = CropBox { left: third.left + 20.0, bottom: third.bottom + 10.0, right: third.right - 40.0, top: third.top - 30.0 };
        let mut session = EditSession::new(source.clone(), pages.len());
        let original = session.plan.clone();

        assert!(session.apply(PageEdit::CropMany { crops: vec![(0, first_crop), (pages.len(), third_crop)] }).unwrap_err().contains("range"));
        assert_eq!(session.plan, original); assert_eq!(session.revision, 0); assert!(!session.can_undo()); assert!(!session.dirty());
        assert!(session.apply(PageEdit::CropMany { crops: vec![(2, third_crop), (0, first_crop)] }).unwrap_err().contains("ascending"));
        assert!(session.apply(PageEdit::CropMany { crops: vec![(0, first_crop), (0, first_crop)] }).unwrap_err().contains("duplicates"));
        assert!(session.apply(PageEdit::CropMany { crops: vec![(0, first)] }).unwrap_err().contains("too small"));
        assert_eq!(session.plan, original); assert_eq!(session.revision, 0); assert!(!session.can_undo());

        session.apply(PageEdit::CropMany { crops: vec![(0, first_crop), (2, third_crop)] }).unwrap();
        assert_eq!(session.revision, 1); assert!(session.can_undo()); assert!(!session.can_redo());
        assert_eq!(session.plan[0].crop, Some(first_crop)); assert_eq!(session.plan[2].crop, Some(third_crop));
        assert_eq!(session.plan[1], original[1]); assert_eq!(session.source, source);
        session.apply(PageEdit::Undo).unwrap(); assert_eq!(session.plan, original); assert_eq!(session.revision, 2); assert!(session.can_redo());
        session.apply(PageEdit::Redo).unwrap(); assert_eq!(session.plan[0].crop, Some(first_crop)); assert_eq!(session.plan[2].crop, Some(third_crop)); assert_eq!(session.revision, 3);
        assert_history_budget(&session);
    }
    #[test]
    fn crop_inherited_intersected_boxes_survive_structural_export_and_invalid_edits_are_atomic() {
        let mut document = Document::load_mem(&sample()).unwrap();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        for id in document.get_pages().values() {
            let page = document.get_object_mut(*id).unwrap().as_dict_mut().unwrap();
            page.remove(b"MediaBox"); page.remove(b"CropBox");
        }
        let parent = document.get_object_mut(root).unwrap().as_dict_mut().unwrap();
        parent.set("MediaBox", vec![20.into(), 30.into(), 612.into(), 792.into()]);
        parent.set("CropBox", vec![0.into(), 50.into(), 600.into(), 900.into()]);
        let first = document.get_pages()[&1];
        assert_eq!(visible_box(&document, first).unwrap(), CropBox { left: 20.0, bottom: 50.0, right: 600.0, top: 792.0 });
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
        let mut session = EditSession::new(bytes.clone(), 6);
        let crop = CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 };
        session.apply(PageEdit::Crop { page: 0, crop }).unwrap();
        let current = session.plan.clone(); let revision = session.revision;
        for invalid in [CropBox { left: f32::NAN, ..crop }, CropBox { right: f32::INFINITY, ..crop }, CropBox { right: 30.5, ..crop }, CropBox { bottom: 20.0, ..crop }, CropBox { top: 60.0, ..crop }] {
            assert!(session.apply(PageEdit::Crop { page: 0, crop: invalid }).is_err());
            assert_eq!(session.plan, current); assert_eq!(session.revision, revision);
        }
        assert!(session.apply(PageEdit::Crop { page: 6, crop }).is_err());
        session.apply(PageEdit::Move { from: 0, to: 5 }).unwrap();
        let output = Document::load_mem(&session.export(Some(&[0, 5])).unwrap()).unwrap();
        assert_eq!(visible_box(&output, output.get_pages()[&2]).unwrap(), crop);
        assert_eq!(visible_box(&output, output.get_pages()[&1]).unwrap(), visible_box(&document, document.get_pages()[&2]).unwrap());
        assert_eq!(output.get_page_content(output.get_pages()[&2]), document.get_page_content(first));
        assert_eq!(session.source, bytes);
    }
    #[test]
    fn reset_crop_many_restores_inherited_source_box_and_is_one_atomic_history_step() {
        let mut document = Document::load_mem(&sample()).unwrap();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        for id in document.get_pages().values() {
            let page = document.get_object_mut(*id).unwrap().as_dict_mut().unwrap();
            page.remove(b"MediaBox"); page.remove(b"CropBox"); page.remove(b"Rotate");
        }
        let parent = document.get_object_mut(root).unwrap().as_dict_mut().unwrap();
        parent.set("MediaBox", vec![20.into(), 30.into(), 612.into(), 792.into()]);
        parent.set("CropBox", vec![0.into(), 50.into(), 600.into(), 900.into()]);
        parent.set("Rotate", 90);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
        let source = bytes.clone();
        let source_document = Document::load_mem(&bytes).unwrap();
        let original_visible = visible_box(&source_document, source_document.get_pages()[&1]).unwrap();
        assert_eq!(original_visible, CropBox { left: 20.0, bottom: 50.0, right: 600.0, top: 792.0 });

        let first_crop = CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 };
        let third_crop = CropBox { left: 40.0, bottom: 70.0, right: 580.0, top: 770.0 };
        let mut session = EditSession::new(bytes, 6);
        session.apply(PageEdit::CropMany { crops: vec![(0, first_crop), (2, third_crop)] }).unwrap();
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        session.apply(PageEdit::Move { from: 0, to: 1 }).unwrap();
        let before_reset = session.plan.clone();
        let before_revision = session.revision;

        session.apply(PageEdit::ResetCropMany { pages: vec![0, 1] }).unwrap();
        let mut expected = before_reset.clone(); expected[0].crop = None; expected[1].crop = None;
        assert_eq!(session.plan, expected, "Only selected session crop fields may change");
        assert_eq!(session.revision, before_revision + 1); assert!(session.can_undo() && !session.can_redo());
        assert_eq!(session.source, source);

        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        let reset_page = output.get_pages()[&2];
        assert_eq!(visible_box(&output, reset_page).unwrap(), original_visible, "Reset must restore the source's inherited visible box");
        let raw_box = |key: &[u8]| output.get_dictionary(reset_page).unwrap().get(key).unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<_>>();
        assert_eq!(raw_box(b"MediaBox"), vec![20.0, 30.0, 612.0, 792.0], "Structural export must retain the exact inherited source MediaBox object");
        assert_eq!(raw_box(b"CropBox"), vec![0.0, 50.0, 600.0, 900.0], "Reset must retain the exact inherited source CropBox object, not its visible intersection");
        assert_eq!(visible_box(&output, output.get_pages()[&3]).unwrap(), third_crop, "An unselected session crop must remain");
        assert_eq!(output.get_dictionary(reset_page).unwrap().get(b"Rotate").unwrap().as_i64().unwrap(), 180, "Reset must preserve edited rotation");

        session.apply(PageEdit::Undo).unwrap(); assert_eq!(session.plan, before_reset); assert!(session.can_redo());
        let no_op_revision = session.revision; let no_op_dirty = session.dirty(); let no_op_undo = session.can_undo();
        session.apply(PageEdit::ResetCropMany { pages: vec![0] }).unwrap();
        assert_eq!(session.revision, no_op_revision); assert_eq!(session.dirty(), no_op_dirty); assert_eq!(session.can_undo(), no_op_undo); assert!(session.can_redo(), "A no-op must retain redo history");
        session.apply(PageEdit::Redo).unwrap(); assert_eq!(session.plan, expected);

        for pages in [vec![], vec![1, 0], vec![1, 1], vec![6]] {
            let plan = session.plan.clone(); let revision = session.revision;
            assert!(session.apply(PageEdit::ResetCropMany { pages }).is_err());
            assert_eq!(session.plan, plan); assert_eq!(session.revision, revision);
        }
        session.mark_saved();
        session.apply(PageEdit::ResetCropMany { pages: vec![2] }).unwrap(); assert!(session.dirty());
        session.apply(PageEdit::Undo).unwrap(); assert!(!session.dirty()); assert_eq!(session.plan, expected);
        assert_eq!(session.source, source);
    }
    #[test]
    fn crop_protected_malformed_and_unsupported_rotations_fail_before_mutation() {
        let crop = CropBox { left: 30.0, bottom: 60.0, right: 590.0, top: 780.0 };
        for kind in ["certified", "signature", "byte_range", "rotation", "boundary", "encrypted", "malformed", "page_count"] {
            let mut document = Document::load_mem(&sample()).unwrap();
            let first = document.get_pages()[&1];
            match kind {
                "certified" => { document.catalog_mut().unwrap().set("Perms", dictionary! {}); }
                "signature" => { document.add_object(dictionary! { "Type" => "Sig" }); }
                "byte_range" => { document.add_object(dictionary! { "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] }); }
                "rotation" => document.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("Rotate", 45),
                "boundary" => document.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("CropBox", vec![0.into(), 0.into(), 0.into(), 10.into()]),
                "encrypted" => {
                    document.trailer.set("ID", vec![Object::string_literal("crop-test-id"), Object::string_literal("crop-test-id")]);
                    let encryption = lopdf::EncryptionVersion::V2 { document: &document, owner_password: "owner", user_password: "", key_length: 128, permissions: lopdf::Permissions::all() };
                    document.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
                }
                _ => {}
            }
            let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap();
            if kind == "malformed" { bytes = b"not a PDF".to_vec(); }
            let mut session = EditSession::new(bytes.clone(), if kind == "page_count" { 5 } else { 6 });
            let original = session.plan.clone();
            assert!(session.apply(PageEdit::Crop { page: 0, crop }).is_err(), "{kind}");
            assert!(session.apply(PageEdit::ResetCropMany { pages: vec![0] }).is_err(), "reset {kind}");
            assert_eq!(session.plan, original); assert_eq!(session.source, bytes); assert_eq!(session.revision, 0);
            assert!(!session.dirty()); assert!(!session.can_undo()); assert!(!session.can_redo());
        }
    }
    #[test]
    fn shared_page_objects_reject_rotation_and_export_without_mutation() {
        let mut document = Document::load_mem(&sample()).unwrap();
        let pages = document.get_pages();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        let mut kids: Vec<_> = pages.values().copied().map(Object::Reference).collect();
        kids[1] = kids[0].clone();
        document.get_object_mut(root).unwrap().as_dict_mut().unwrap().set("Kids", kids);
        let mut source = Vec::new(); document.save_to(&mut source).unwrap();
        assert_eq!(Document::load_mem(&source).unwrap().get_pages().len(), 6);
        let mut session = EditSession::new(source.clone(), 6);
        let original_plan = session.plan.clone();
        let error = session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap_err();
        assert!(error.contains("repeats a page object"));
        assert!(session.export(None).unwrap_err().contains("repeats a page object"));
        assert!(session.export(Some(&[0])).is_err());
        assert_eq!(session.source, source);
        assert_eq!(session.plan, original_plan);
        assert_eq!(session.saved, original_plan);
        assert_eq!(session.revision, 0);
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        assert!(!session.dirty());
        let mut ordinary = EditSession::new(sample(), 6);
        ordinary.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        assert!(ordinary.export(None).is_ok());
        assert_eq!(ordinary.plan[0].turns, 1);
        assert_eq!(ordinary.plan[1].turns, 0);
    }
    #[test]
    fn history_evicts_oldest_snapshots_and_preserves_current_source_and_saved_state() {
        let source = sample();
        let mut session = EditSession::new(source.clone(), 6);
        session.history_budget = snapshot_bytes(&session.plan) * 3;
        let saved = session.saved.clone();
        let mut states = vec![session.plan.clone()];
        for _ in 0..17 {
            session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
            states.push(session.plan.clone());
            assert_history_budget(&session);
        }
        assert_eq!(session.undo.len(), 3);
        assert_eq!(session.plan, states[17]);
        assert_eq!(session.source, source);
        assert_eq!(session.saved, saved);
        for index in (14..17).rev() {
            session.apply(PageEdit::Undo).unwrap();
            assert_eq!(session.plan, states[index]);
            assert_history_budget(&session);
        }
        assert!(!session.can_undo());
        assert!(session.apply(PageEdit::Undo).is_err());
        for state in states.iter().take(18).skip(15) {
            session.apply(PageEdit::Redo).unwrap();
            assert_eq!(&session.plan, state);
            assert_history_budget(&session);
        }
        assert!(!session.can_redo());
        assert_eq!(session.source, source);
        assert_eq!(session.saved, saved);
    }
    #[test]
    fn bounded_history_branches_clear_redo_and_keep_saved_baseline() {
        let mut session = EditSession::new(sample(), 6);
        session.history_budget = snapshot_bytes(&session.plan) * 2;
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        session.mark_saved();
        let saved = session.plan.clone();
        session.apply(PageEdit::Delete { pages: vec![1, 2, 3] }).unwrap();
        session.apply(PageEdit::Undo).unwrap();
        assert!(!session.dirty());
        assert!(session.can_redo());
        assert_history_budget(&session);
        session.apply(PageEdit::Rotate { pages: vec![1], clockwise: true }).unwrap();
        assert!(!session.can_redo());
        assert!(session.dirty());
        assert_eq!(session.saved, saved);
        assert_history_budget(&session);
        session.apply(PageEdit::Undo).unwrap();
        assert_eq!(session.plan, saved);
        assert!(!session.dirty());
        assert_history_budget(&session);
    }
    #[test]
    fn oversized_snapshot_is_dropped_without_rejecting_or_reverting_edit() {
        let source = sample();
        let mut session = EditSession::new(source.clone(), 6);
        session.history_budget = snapshot_bytes(&session.plan) - 1;
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        assert_eq!(session.plan[0].turns, 1);
        assert!(session.dirty());
        assert!(!session.can_undo());
        assert!(!session.can_redo());
        assert_eq!(session.source, source);
        assert_eq!(session.saved[0].turns, 0);
        assert_history_budget(&session);
    }
    #[test]
    fn history_accounts_for_reserved_capacity_and_both_stacks() {
        let mut session = EditSession::new(sample(), 6);
        let mut roomy = Vec::with_capacity(100);
        roomy.push(PageSpec { source: 0, turns: 0, crop: None, notes: Vec::new() });
        let roomy_bytes = snapshot_bytes(&roomy);
        assert!(roomy_bytes > std::mem::size_of::<PageSpec>() * roomy.len());
        let small = session.plan.clone();
        session.history_budget = snapshot_bytes(&small);
        session.undo.push_back(roomy);
        session.redo.push_back(small);
        session.history_bytes = roomy_bytes + session.history_budget;
        let current = session.plan.clone();
        session.trim_history();
        assert!(!session.can_undo());
        assert!(session.can_redo());
        assert_eq!(session.plan, current);
        assert_history_budget(&session);
        session.history_budget = 0;
        session.trim_history();
        assert!(!session.can_redo());
        assert_eq!(session.plan, current);
        assert_history_budget(&session);
    }
    #[test]
    fn undo_redo_and_branch_preserve_source() {
        let bytes = sample(); let mut session = EditSession::new(bytes.clone(), 6);
        session.apply(PageEdit::Rotate { pages: vec![0, 0], clockwise: true }).unwrap();
        assert_eq!(session.plan[0].turns, 1);
        session.apply(PageEdit::Delete { pages: vec![1, 3] }).unwrap();
        assert_eq!(session.plan.iter().map(|p| p.source).collect::<Vec<_>>(), vec![0, 2, 4, 5]);
        session.apply(PageEdit::Undo).unwrap(); assert_eq!(session.plan.len(), 6);
        session.apply(PageEdit::Redo).unwrap(); assert_eq!(session.plan.len(), 4);
        session.apply(PageEdit::Undo).unwrap();
        session.apply(PageEdit::Move { from: 5, to: 0 }).unwrap();
        assert!(!session.can_redo()); assert_eq!(session.source, bytes);
        assert!(session.apply(PageEdit::Delete { pages: (0..6).collect() }).is_err());
        assert!(session.apply(PageEdit::Rotate { pages: vec![99], clockwise: true }).is_err());
    }
    #[test]
    fn output_preserves_order_rotation_text_and_safe_save() {
        let mut session = EditSession::new(sample(), 6);
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: false }).unwrap();
        session.apply(PageEdit::Move { from: 5, to: 0 }).unwrap();
        let bytes = session.export(Some(&[0, 1])).unwrap();
        let document = Document::load_mem(&bytes).unwrap();
        assert_eq!(document.get_pages().len(), 2);
        assert!(document.extract_text(&[1]).unwrap().contains("Ready for a real document"));
        assert!(document.extract_text(&[2]).unwrap().contains("A place for your PDFs"));
        let second = document.get_pages()[&2];
        assert_eq!(inherited(&document, second, b"Rotate").unwrap().unwrap().as_i64().unwrap(), 270);
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("copy.pdf");
        write_new_file(&path, &bytes).unwrap(); assert!(write_new_file(&path, b"overwrite").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
    #[test]
    fn inherited_page_attributes_survive_reordering() {
        let mut document = Document::load_mem(&sample()).unwrap();
        let root = document.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
        let first = document.get_pages()[&1];
        let resources = document.get_dictionary(first).unwrap().get(b"Resources").unwrap().clone();
        for id in document.get_pages().values() {
            let page = document.get_object_mut(*id).unwrap().as_dict_mut().unwrap();
            page.remove(b"Resources"); page.remove(b"MediaBox");
        }
        let parent = document.get_object_mut(root).unwrap().as_dict_mut().unwrap();
        parent.set("Resources", resources); parent.set("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()]); parent.set("Rotate", 90);
        let mut bytes=Vec::new(); document.save_to(&mut bytes).unwrap();
        let mut session = EditSession::new(bytes, 6); session.apply(PageEdit::Move { from: 0, to: 5 }).unwrap();
        let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
        for id in output.get_pages().values() {
            assert!(output.get_dictionary(*id).unwrap().has(b"Resources"));
            assert_eq!(inherited(&output, *id, b"Rotate").unwrap().unwrap().as_i64().unwrap(), 90);
        }
    }
    #[test]
    fn parser_page_count_disagreement_blocks_edits_and_export() {
        for count in [5, 7] {
            let mut session = EditSession::new(sample(), count);
            let original = session.plan.clone();
            assert!(session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap_err().contains("disagree"));
            assert!(session.export(None).unwrap_err().contains("disagree"));
            assert!(session.export(Some(&[0])).unwrap_err().contains("disagree"));
            assert_eq!(session.plan, original); assert_eq!(session.revision, 0); assert!(!session.can_undo());
        }
        let mut valid = EditSession::new(sample(), 6);
        valid.apply(PageEdit::Delete { pages: vec![0] }).unwrap();
        assert_eq!(Document::load_mem(&valid.export(None).unwrap()).unwrap().get_pages().len(), 5);
    }
    #[test]
    fn extreme_rotations_export_without_overflow() {
        for rotation in [i64::MAX / 90 * 90, i64::MIN / 90 * 90] {
            let mut document = Document::load_mem(&sample()).unwrap();
            let first = document.get_pages()[&1];
            document.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("Rotate", rotation);
            let mut source = Vec::new(); document.save_to(&mut source).unwrap();
            let mut session = EditSession::new(source, 6);
            session.apply(PageEdit::Rotate { pages: vec![0], clockwise: false }).unwrap();
            let output = Document::load_mem(&session.export(None).unwrap()).unwrap();
            let actual = inherited(&output, output.get_pages()[&1], b"Rotate").unwrap().unwrap().as_i64().unwrap();
            assert_eq!(actual, (rotation.rem_euclid(360) + 270) % 360);
        }
    }
    const ORPHAN: &[u8] = b"SM2ORPHAN42";
    const OLD_REVISION: &[u8] = b"SM2OLDREV42";
    const DELETED: &[u8] = b"SM2DELETED42";

    fn literal_pdf(objects: &[String], update: &[(u32, String)]) -> Vec<u8> {
        let mut bytes = b"%PDF-1.7\n".to_vec(); let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() { offsets.push(bytes.len()); bytes.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes()); }
        let startxref = bytes.len(); let size = objects.len() + 1;
        bytes.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
        for offset in offsets { bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes()); }
        bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{startxref}\n%%EOF\n").as_bytes());
        if update.is_empty() { return bytes; }
        let mut entries = Vec::new();
        for (number, body) in update { entries.push((*number, bytes.len())); bytes.extend_from_slice(format!("{number} 0 obj\n{body}\nendobj\n").as_bytes()); }
        let next = bytes.len(); let size = update.iter().map(|(number, _)| *number as usize + 1).max().unwrap().max(size);
        bytes.extend_from_slice(b"xref\n");
        for (number, offset) in entries { bytes.extend_from_slice(format!("{number} 1\n{offset:010} 00000 n \n").as_bytes()); }
        bytes.extend_from_slice(format!("trailer\n<< /Size {size} /Root 1 0 R /Prev {startxref} >>\nstartxref\n{next}\n%%EOF\n").as_bytes());
        bytes
    }

    fn content(text: &str) -> String { let data = format!("BT /F1 12 Tf 20 100 Td ({text}) Tj ET"); format!("<< /Length {} >>\nstream\n{data}\nendstream", data.len()) }

    /// Two pages; object 8 is unreferenced and carries ORPHAN; an incremental update points page 1 at a
    /// new content stream, leaving the superseded one (object 5, OLD_REVISION) unreferenced.
    fn leaky_source() -> Vec<u8> {
        literal_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>".into(),
            content("SM2OLDREV42"),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 7 0 R >>".into(),
            content("page two"),
            content("SM2ORPHAN42"),
        ], &[
            (9, content("revised")),
            (4, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 9 0 R >>".into()),
        ])
    }

    fn carries(bytes: &[u8], marker: &[u8]) -> bool {
        bytes.windows(marker.len()).any(|window| window == marker) || Document::load_mem(bytes).unwrap().objects.values().any(|object| matches!(object, Object::Stream(stream) if stream.decompressed_content().unwrap_or_else(|_| stream.content.clone()).windows(marker.len()).any(|window| window == marker)))
    }

    #[test]
    fn copies_never_carry_unreachable_source_objects() {
        let source = leaky_source();
        // Positive control: the source carries both markers, and a plain lopdf load and save (what
        // rotate, crop and comment copies did before) carries them into the copy.
        let mut unpruned = Vec::new(); Document::load_mem(&source).unwrap().save_to(&mut unpruned).unwrap();
        assert!(carries(&source, ORPHAN) && carries(&source, OLD_REVISION));
        assert!(carries(&unpruned, ORPHAN) && carries(&unpruned, OLD_REVISION), "the leak must exist without pruning, or this test proves nothing");
        let original = Document::load_mem(&source).unwrap();
        let original_pages: Vec<_> = original.get_pages().into_values().collect();
        let edits: [(&str, fn(&mut EditSession)); 4] = [
            ("rotate", |session| session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap()),
            ("crop", |session| session.apply(PageEdit::Crop { page: 0, crop: CropBox { left: 10.0, bottom: 10.0, right: 190.0, top: 190.0 } }).unwrap()),
            ("comment", |session| { let plan = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("note")).unwrap(); session.commit_comments(plan); }),
            ("move", |session| session.apply(PageEdit::Move { from: 1, to: 0 }).unwrap()),
        ];
        for (name, edit) in edits {
            let mut session = EditSession::new(source.clone(), 2);
            edit(&mut session);
            let mut outputs = vec![session.export(None).unwrap()];
            if let Some(noted) = session.note_source(&session.plan).unwrap() { outputs.push(noted); }
            for output in outputs {
                assert!(!carries(&output, ORPHAN) && !carries(&output, OLD_REVISION), "{name}: an unreachable source object reached the copy");
                let document = Document::load_mem(&output).unwrap();
                assert_eq!(document.get_pages().len(), 2, "{name}");
                for (index, spec) in session.plan.iter().enumerate() {
                    let page = document.get_pages()[&(index as u32 + 1)];
                    assert_eq!(document.get_page_content(page), original.get_page_content(original_pages[spec.source]), "{name}: page content must be unchanged");
                }
                assert!(document.extract_text(&[if name == "move" { 2 } else { 1 }]).unwrap().contains("revised"), "{name}");
            }
        }
    }

    #[test]
    fn removed_pages_referenced_from_kept_pages_or_the_catalog_do_not_survive_export() {
        let source = literal_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /SMCustom 6 0 R /SMTree 2 0 R >>".into(),
            "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R /SMLink [6 0 R 8 0 R 9 0 R 10 0 R 12 0 R 13 0 R] >>".into(),
            content("kept page"),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 7 0 R >>".into(),
            content("SM2DELETED42"),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [6 0 R /Fit] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D [6 0 R /Fit] >> >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 11 0 R >>".into(),
            "<< /S /GoTo /D [6 0 R /XYZ 0 0 0] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>".into(),
            "<< /Title (bookmark-like) /Dest [6 0 R /Fit] >>".into(),
        ], &[]);
        let mut session = EditSession::new(source.clone(), 2);
        session.apply(PageEdit::Delete { pages: vec![1] }).unwrap();
        for output in [session.export(None).unwrap(), EditSession::new(source.clone(), 2).export(Some(&[0])).unwrap()] {
            assert!(!carries(&output, DELETED), "a removed page survived through a custom reference");
            let document = Document::load_mem(&output).unwrap();
            assert_eq!(document.get_pages().len(), 1);
            assert!(document.extract_text(&[1]).unwrap().contains("kept page"));
            assert_eq!(document.objects.values().filter(|object| object.as_dict().ok().and_then(|dict| dict.get(b"Type").ok()).and_then(|value| value.as_name().ok()) == Some(b"Page".as_slice())).count(), 1, "only the kept page object remains");
            // Link targets: no destination is left as [null ...]; links to the removed page are gone,
            // the link to the kept page keeps its destination, and a non-link keeps its other keys.
            let kept = document.get_pages()[&1];
            let mut links = Vec::new();
            for object in document.objects.values() {
                let Ok(dict) = object.as_dict() else { continue };
                for key in [b"Dest".as_slice(), b"D"] { if let Ok(destination) = dict.get(key) { assert_ne!(destination.as_array().unwrap().first(), Some(&Object::Null), "dead destination left as null"); } }
                if dict.get(b"Subtype").and_then(Object::as_name).is_ok_and(|name| name == b"Link") { links.push(dict.clone()); }
                if dict.get(b"Title").is_ok() { assert!(!dict.has(b"Dest"), "the dead /Dest is dropped, the dictionary kept"); }
            }
            assert_eq!(links.len(), 1, "only the link to the kept page remains");
            assert_eq!(links[0].get(b"Dest").unwrap().as_array().unwrap()[0], Object::Reference(kept));
            assert!(!document.objects.values().any(|object| object.as_dict().is_ok_and(|dict| dict.get(b"S").and_then(Object::as_name).is_ok_and(|name| name == b"GoTo"))), "dead GoTo actions are unreachable");
        }
    }

    #[test]
    fn page_reference_removal_resolves_indirect_destinations_keeps_bare_links_and_cleans_annots() {
        let source = literal_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R >>".into(),
            "<< /Type /Pages /Kids [4 0 R 6 0 R] /Count 2 >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R /Annots [8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R 17 0 R 18 0 R] >>".into(),
            content("kept page"),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 3 0 R >> >> /Contents 7 0 R >>".into(),
            content("removed page"),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest 14 0 R >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D 14 0 R >> >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A 15 0 R >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [4 0 R /Fit] >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest [6 0 R /Fit] >>".into(),
            "[6 0 R /Fit]".into(),
            "<< /S /GoTo /D 14 0 R >>".into(),
            "[4 0 R /Fit]".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest 16 0 R >>".into(),
            "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /Dest 16 0 R >>".into(),
        ], &[]);
        let mut document = Document::load_mem(&source).unwrap();
        remove_page_references(&mut document, HashSet::from([(6, 0)]));
        let annots = document.get_dictionary((4, 0)).unwrap().get(b"Annots").unwrap().as_array().unwrap().clone();
        assert_eq!(annots, vec![Object::Reference((11, 0)), Object::Reference((12, 0)), Object::Reference((17, 0)), Object::Reference((18, 0))], "dead links leave no null behind in /Annots");
        for number in [8, 9, 10] {
            let link = document.get_dictionary((number, 0)).unwrap();
            assert!(!link.has(b"Dest") && !link.has(b"A"), "object {number}: an indirect destination to the removed page is dropped, not left as [null ...]");
        }
        assert!(document.get_dictionary((11, 0)).unwrap().get(b"Subtype").is_ok_and(|value| value.as_name().is_ok_and(|name| name == b"Link")), "a link that never had a destination or action is left untouched");
        for number in [17, 18] { assert_eq!(document.get_dictionary((number, 0)).unwrap().get(b"Dest").unwrap(), &Object::Reference((16, 0)), "link {number} keeps its indirect destination"); }
        assert_eq!(document.get_object((16, 0)).unwrap().as_array().unwrap()[0], Object::Reference((4, 0)), "the shared array to the kept page is intact");
        assert_eq!(document.get_dictionary((12, 0)).unwrap().get(b"Dest").unwrap().as_array().unwrap()[0], Object::Reference((4, 0)));
    }

    #[test]
    fn pruned_export_of_the_1500_page_fixture_stays_fast() {
        let source = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-corpus/synthetic-text-1500.pdf")).unwrap();
        let count = Document::load_mem(&source).unwrap().get_pages().len();
        let mut session = EditSession::new(source, count);
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        let started = std::time::Instant::now();
        let output = session.export(None).unwrap();
        let elapsed = started.elapsed();
        eprintln!("SM2-PRUNE 1500-page rotate export: {elapsed:?}, {} bytes", output.len());
        assert_eq!(Document::load_mem(&output).unwrap().get_pages().len(), count);
        // Measured about 130 ms in a debug build; 5 s leaves room for a loaded machine while still
        // catching a regression to a much slower walk.
        assert!(elapsed < std::time::Duration::from_secs(5), "{elapsed:?}");
    }

    #[test]
    fn unsupported_structures_fail_before_mutation() {
        let mut document = Document::load_mem(&sample()).unwrap();
        document.catalog_mut().unwrap().set("AcroForm", dictionary!{});
        let mut bytes=Vec::new(); document.save_to(&mut bytes).unwrap();
        let mut session=EditSession::new(bytes, 6);
        assert!(session.apply(PageEdit::Delete { pages: vec![0] }).is_err()); assert_eq!(session.plan.len(),6);
        session.apply(PageEdit::Rotate { pages: vec![0], clockwise: true }).unwrap();
        let mut document=Document::load_mem(&sample()).unwrap(); document.catalog_mut().unwrap().set("Perms", dictionary!{});
        let mut bytes=Vec::new(); document.save_to(&mut bytes).unwrap();
        assert!(EditSession::new(bytes,6).export(None).is_err());
    }
    fn nested_pdf(depth: usize, dictionaries: bool) -> Vec<u8> {
        let (open, close) = if dictionaries { ("<< /K ", " >>") } else { ("[", "]") };
        let deep = format!("{}4 0 R{}", open.repeat(depth), close.repeat(depth));
        let bodies = ["<< /Type /Catalog /Pages 2 0 R >>".to_string(), "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 200 200] >>".into(), "<< /Type /Page /Parent 2 0 R /Resources << >> /Deep 5 0 R >>".into(), "<< /Type /Page /Parent 2 0 R /Resources << >> >>".into(), deep];
        let mut pdf = b"%PDF-1.7\n".to_vec(); let mut offsets = Vec::new();
        for (index, body) in bodies.iter().enumerate() { offsets.push(pdf.len()); pdf.extend(format!("{} 0 obj\n{body}\nendobj\n", index + 1).bytes()); }
        let xref = pdf.len(); pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).bytes());
        for offset in offsets { pdf.extend(format!("{offset:010} 00000 n \n").bytes()); }
        pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).bytes()); pdf
    }
    fn nesting(object: &Object) -> usize { match object { Object::Array(values) => 1 + values.iter().map(nesting).max().unwrap_or(0), Object::Dictionary(dict) => 1 + dict.iter().map(|(_, value)| nesting(value)).max().unwrap_or(0), _ => 0 } }
    #[test]
    fn hostile_nesting_depth_is_bounded_by_the_parser_and_save_copy_fits_the_production_stack() {
        // lopdf parses on rayon pool threads and pdf-worker parses trailers and content; both run on the 2 MiB
        // std default. Measured peak at 100 levels: ~1.95 MiB debug, <= 259 KiB release. Run everything on a
        // private pool: release at a quarter of production (asserts >= 4x margin), debug at twice production.
        const PRODUCTION_STACK: usize = 2 * 1024 * 1024;
        let stack = if cfg!(debug_assertions) { PRODUCTION_STACK * 2 } else { PRODUCTION_STACK / 4 };
        let pool = rayon::ThreadPoolBuilder::new().num_threads(2).stack_size(stack).thread_name(|index| format!("sm7-nesting-{index}")).build().unwrap();
        pool.install(|| {
            let wrap = |depth: usize, dictionaries: bool, leaf: &str| { let (open, close) = if dictionaries { ("<< /K ", " >>") } else { ("[", "]") }; format!("{}{leaf}{}", open.repeat(depth), close.repeat(depth)) };
            for dictionaries in [false, true] {
                let trailer = String::from_utf8(nested_pdf(1, dictionaries)).unwrap().replace("/Root 1 0 R >>", &format!("/Root 1 0 R /Deep {} >>", wrap(99, dictionaries, "1")));
                let mut document = crate::pdf_load::load(trailer.as_bytes()).unwrap();
                assert_eq!(nesting(&Object::Dictionary(document.trailer.clone())), 100, "the trailer parses on the calling thread");
                document.save_to(&mut Vec::new()).unwrap();
                assert_eq!(lopdf::content::Content::decode(format!("{} Tj", wrap(100, dictionaries, "1")).as_bytes()).unwrap().operations.len(), 1);
                let _ = lopdf::content::Content::decode(format!("{} Tj", wrap(100_000, dictionaries, "1")).as_bytes());
                let loaded = |depth| crate::pdf_load::load(&nested_pdf(depth, dictionaries)).ok().and_then(|document| document.objects.get(&(5, 0)).map(nesting));
                assert_eq!(loaded(100), Some(100), "lopdf 0.45 accepts 100 levels of direct nesting");
                assert_eq!(loaded(101), None, "lopdf 0.45 must refuse a 101-level object; a parser upgrade that drops MAX_NESTING_DEPTH reopens unbounded recursion in null_references::scrub and the other Object walkers");
                let export = EditSession::new(nested_pdf(100, dictionaries), 2).export(Some(&[0])).unwrap();
                let copy = Document::load_mem(&export).unwrap();
                assert_eq!(copy.get_pages().len(), 1);
                let mut deepest = copy.objects.values().map(nesting).max().unwrap();
                assert!(deepest >= 100, "the deep object survives the save-copy");
                let mut value = copy.objects.values().find(|object| nesting(object) == deepest).unwrap();
                while deepest > 0 { value = match value { Object::Array(values) => &values[0], Object::Dictionary(dict) => dict.get(b"K").unwrap(), _ => unreachable!() }; deepest -= 1; }
                assert_eq!(value, &Object::Null, "scrub reached the bottom and nulled the removed page reference");
                for depth in [101, 100_000] { let _ = EditSession::new(nested_pdf(depth, dictionaries), 2).export(Some(&[0])); }
            }
        });
    }
}
