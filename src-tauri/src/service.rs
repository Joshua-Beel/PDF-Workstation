use std::{collections::{HashMap, HashSet, VecDeque}, io::{Cursor, Write}, path::PathBuf, sync::{Arc, Mutex, mpsc, OnceLock}};
use pdfium_render::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use crate::editor::{CropBox, EditSession, PageEdit, PageSpec, write_new_file};
use crate::combine::CopyOperation;
use crate::page_image::PageImageFormat;

#[derive(Clone, Copy, Deserialize)]
pub struct CropRect { pub x: f64, pub y: f64, pub width: f64, pub height: f64 }
#[derive(Clone, Copy, Deserialize)]
pub struct CropInsets { pub top: f64, pub right: f64, pub bottom: f64, pub left: f64 }
#[derive(Clone, Copy, Deserialize)]
pub struct CombineSource { pub id: u64, pub revision: u64 }
enum CommentMutation { Create(u16, CropRect, String), Update(String, String), Delete(String), CreateHighlight(u16, CropRect, Option<String>), CreateTextHighlight(u16, usize, usize, Option<String>), UpdateHighlight(String, Option<String>), DeleteHighlight(String) }

#[derive(Clone, Serialize)]
pub struct PageSize { width: f32, height: f32 }
#[derive(Clone, Serialize)]
pub struct DocumentInfo { id: u64, name: String, path: String, pages: Vec<PageSize>, revision: u64, dirty: bool, can_undo: bool, can_redo: bool }
impl DocumentInfo {
    #[cfg(all(test, windows))]
    pub(crate) fn ocr_request(&self, page: u16) -> crate::ocr::OcrPageRequest {
        crate::ocr::OcrPageRequest { document_id: self.id, revision: self.revision, page }
    }
}
#[derive(Serialize)]
pub struct SavedCopy { path: String, document: DocumentInfo }
#[allow(dead_code)]
#[derive(Serialize)]
pub struct RecoveredDocument { pub document: DocumentInfo, pub current_page: u32 }
#[derive(Debug)]
pub struct PageImagePreflight { pub suggested_name: String }
#[derive(Debug)]
pub struct RasterRedactionPreflight { pub suggested_name: String }
#[derive(Debug)]
pub struct SearchableOcrPreflight { pub suggested_name: String, pub page_count: usize }
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PageImageReceipt { path: String, document_id: u64, revision: u64, page: u16, dpi: u16, width: u32, height: u32, format: PageImageFormat }
#[derive(Serialize)]
pub struct BookmarkInfo { title: String, page: Option<usize>, depth: usize }
#[derive(Serialize)]
pub struct BookmarkList { items: Vec<BookmarkInfo>, truncated: bool }
const MAX_READ_TEXT_BYTES: usize = 1_048_576;
const MAX_PASSWORD_BYTES: usize = 1024;
const STALE_PAGE_PLAN: &str = "Document changed. Reload the page list and try again.";
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpenResult {
    Opened { document: DocumentInfo, recovery: Option<RecoveryOffer> },
    PasswordRequired { request_id: u64, name: String, incorrect: bool },
}
pub struct PrintSnapshotInfo { pub token: u64, pub pages: usize, pub name: String, cleanup: RequestSender }
impl Drop for PrintSnapshotInfo {
    fn drop(&mut self) {
        let (reply, _) = oneshot::channel();
        let _ = self.cleanup.send_cleanup(Request::EndPrint(self.token, reply));
    }
}
pub struct PrintBitmap { pub width: u32, pub height: u32, pub bgra: Vec<u8> }
type Reply<T> = oneshot::Sender<Result<T, String>>;
#[cfg(test)]
#[derive(Default, Debug)]
struct RenderWork { dequeued: usize, skipped_closed: usize, rendered: usize }
#[cfg(test)]
#[derive(Default, Debug)]
struct BookmarkWork { dequeued: usize, visited: usize }
#[cfg(test)]
#[derive(Default, Debug)]
struct OcrWork { dequeued: usize, preflighted: usize, rendered: usize, converted: usize }
#[cfg(test)]
#[derive(Clone, Debug, PartialEq)]
struct OcrWorkerState {
    source: Vec<u8>,
    plan: Vec<PageSpec>,
    revision: u64,
    dirty: bool,
    can_undo: bool,
    can_redo: bool,
    cache_entries: Vec<(CacheKey, usize)>,
    cache_weight: usize,
    note_documents: Vec<(u64, u64)>,
}
enum UnclaimedReply { Document(u64), Password(u64) }
struct ReplyLease<T> { value: Option<T>, cleanup: Option<UnclaimedReply>, sender: RequestSender }
impl<T> ReplyLease<T> {
    fn new(value: T, cleanup: UnclaimedReply, sender: RequestSender) -> Self { Self { value: Some(value), cleanup: Some(cleanup), sender } }
    fn accept(mut self) -> T { self.cleanup = None; self.value.take().expect("A reply lease owns its value until acceptance") }
}
impl<T> std::ops::Deref for ReplyLease<T> {
    type Target = T;
    fn deref(&self) -> &T { self.value.as_ref().expect("An unaccepted reply lease owns its value") }
}
impl<T> Drop for ReplyLease<T> {
    fn drop(&mut self) {
        if let Some(cleanup) = self.cleanup.take() {
            let (reply, _) = oneshot::channel();
            let request = match cleanup { UnclaimedReply::Document(id) => Request::Close(id, reply), UnclaimedReply::Password(id) => Request::CancelPassword(id, reply) };
            let _ = self.sender.send_cleanup(request);
        }
    }
}
enum Request {
    #[cfg(test)]
    BacklogGate(mpsc::Receiver<Vec<Request>>, Reply<()>),
    #[cfg(test)]
    RenderWorkForDocument(u64, Reply<RenderWork>),
    #[cfg(test)]
    BookmarkWorkForDocument(u64, Reply<BookmarkWork>),
    #[cfg(test)]
    OcrWorkForDocument(u64, Reply<OcrWork>),
    #[cfg(test)]
    OcrWorkerState(u64, Reply<OcrWorkerState>),
    #[cfg(test)]
    OpenDocumentsForPath(PathBuf, Reply<Vec<u64>>),
    #[cfg(test)]
    PasswordRequestsForPath(PathBuf, Reply<Vec<u64>>),
    #[cfg(test)]
    EnginePageLabels(u64, Reply<Vec<Option<String>>>),
    Open(PathBuf, Reply<ReplyLease<DocumentInfo>>),
    #[allow(dead_code)]
    CheckpointRecovery(PathBuf, u64, u64, u32, Reply<()>),
    #[allow(dead_code)]
    RestoreRecovery(PathBuf, PathBuf, Reply<Option<ReplyLease<RecoveredDocument>>>),
    KeepRecoveredEdits(u64, u64, Reply<RecoveredDocument>),
    OpenOriginal(u64, u64, Reply<DocumentInfo>),
    DiscardRecovery(u64, u64, Reply<()>),
    BeginOpen(PathBuf, Reply<ReplyLease<OpenResult>>),
    Unlock(u64, String, Reply<ReplyLease<OpenResult>>),
    CancelPassword(u64, Reply<()>),
    BeginPrint(u64, u64, Reply<PrintSnapshotInfo>),
    PrintRender(u64, usize, u32, u32, Reply<PrintBitmap>),
    EndPrint(u64, Reply<()>),
    PreflightPageImage(u64, u64, u16, u16, PageImageFormat, Reply<PageImagePreflight>),
    ExportPageImage(u64, u64, u16, u16, PageImageFormat, PathBuf, Reply<PageImageReceipt>),
    PreflightRasterRedaction(u64, u64, u16, Vec<crate::raster_redaction::RasterRedactionRect>, Reply<RasterRedactionPreflight>),
    RasterRedact(u64, u64, u16, Vec<crate::raster_redaction::RasterRedactionRect>, PathBuf, Reply<ReplyLease<SavedCopy>>),
    #[cfg(windows)]
    PreflightSearchableOcr(u64, u64, Reply<SearchableOcrPreflight>),
    #[cfg(windows)]
    PublishSearchableOcr(u64, u64, PathBuf, crate::searchable_ocr_job::PreparedSearchableOcr, Reply<ReplyLease<SavedCopy>>),
    #[cfg(windows)]
    OcrRaster(crate::ocr::OcrPageRequest, crate::ocr_process::OcrCancellation, crate::ocr_process::OcrWorkerHold, Reply<crate::ocr::OcrPageRaster>),
    #[cfg(windows)]
    ValidateOcrResult(crate::ocr::OcrPageRequest, crate::ocr_process::OcrWorkerHold, Reply<()>),
    Render(u64, u16, i32, Reply<Vec<u8>>),
    AdmittedRender(u64, u16, i32, u64),
    Text(u64, u16, u64, Reply<String>),
    TextGeometry(u64, u16, u64, Reply<crate::text_geometry::PageTextGeometry>),
    Bookmarks(u64, u64, Reply<BookmarkList>),
    PageLabels(u64, u64, Reply<crate::page_labels::DocumentPageLabels>),
    Properties(u64, u64, Reply<crate::document_properties::DocumentProperties>),
    FormFields(u64, u64, Reply<crate::forms::FormFields>),
    CheckFormCopy(u64, u64, Vec<crate::forms::FieldValue>, Reply<()>),
    FillFormCopy(u64, u64, Vec<crate::forms::FieldValue>, PathBuf, Reply<ReplyLease<SavedCopy>>),
    CreateImagePdf(PathBuf, PathBuf, crate::image_pdf::ImagePdfOptions, Reply<ReplyLease<SavedCopy>>),
    CreateMultiImagePdf(crate::image_pdf::PreparedMultiImagePdf, PathBuf, Reply<ReplyLease<SavedCopy>>),
    InspectImageReplacement(u64, u64, u16, Reply<crate::image_edit::ImageEditTargetInfo>),
    ReplaceImageCopy(u64, u64, u16, crate::image_edit::ImageEditTargetInfo, Vec<u8>, PathBuf, Reply<ReplyLease<SavedCopy>>),
    InspectTextReplacement(u64, u64, u16, Reply<TextReplacementInspection>),
    ReplaceTextCopy(u64, u64, u16, TextReplacementInspection, String, PathBuf, Reply<ReplyLease<SavedCopy>>),
    Close(u64, Reply<()>),
    Edit(u64, PageEdit, Reply<DocumentInfo>),
    DurableEdit(u64, PageEdit, u32, Reply<DocumentInfo>),
    DurableEditAt(u64, u64, PageEdit, u32, Reply<DocumentInfo>),
    SaveAt(u64, u64, Option<Vec<usize>>, PathBuf, Reply<SavedCopy>),
    CheckRevision(u64, u64, Reply<()>),
    Crop(u64, u16, u64, CropRect, Reply<DocumentInfo>),
    DurableCrop(u64, u16, u64, CropRect, u32, Reply<DocumentInfo>),
    CropPages(u64, Vec<u16>, u64, CropInsets, Reply<DocumentInfo>),
    DurableCropPages(u64, Vec<u16>, u64, CropInsets, u32, Reply<DocumentInfo>),
    ResetCrops(u64, Vec<u16>, u64, Reply<DocumentInfo>),
    DurableResetCrops(u64, Vec<u16>, u64, u32, Reply<DocumentInfo>),
    Comments(u64, u64, Reply<crate::comments::CommentList>),
    Annotations(u64, u64, Reply<crate::comments::AnnotationList>),
    Comment(u64, u64, CommentMutation, Reply<DocumentInfo>),
    DurableComment(u64, u64, CommentMutation, u32, Reply<DocumentInfo>),
    Save(u64, Option<Vec<usize>>, PathBuf, Reply<SavedCopy>),
    Split(u64, u64, usize, PathBuf, Reply<crate::split::SplitOutput>),
    CheckCombine(CombineSource, CombineSource, Reply<()>),
    Combine(CombineSource, CombineSource, PathBuf, Reply<ReplyLease<SavedCopy>>),
    CheckInsertion(CombineSource, CombineSource, usize, Reply<()>),
    InsertPages(CombineSource, CombineSource, usize, PathBuf, Reply<ReplyLease<SavedCopy>>),
    CheckReplacement(CombineSource, CombineSource, usize, usize, Reply<()>),
    ReplacePages(CombineSource, CombineSource, usize, usize, PathBuf, Reply<ReplyLease<SavedCopy>>),
    CreateCopy(CombineSource, CombineSource, CopyOperation, PathBuf, Reply<ReplyLease<SavedCopy>>),
}
#[derive(Default)]
struct RequestQueue { deferred: VecDeque<Request>, cleanup_overtakes: usize, pending: Arc<std::sync::atomic::AtomicUsize> }
impl RequestQueue {
    fn received(&self, request: Request) -> Request {
        if !matches!(request, Request::Render(..)|Request::AdmittedRender(..)) { let _ = self.pending.fetch_update(std::sync::atomic::Ordering::AcqRel, std::sync::atomic::Ordering::Acquire, |n| n.checked_sub(1)); }
        request
    }
    fn next(&mut self, receiver: &mpsc::Receiver<Request>) -> Result<Request, mpsc::RecvError> {
        // Only viewer renders may be overtaken. Every other command preserves its FIFO boundary.
        // Two cleanup overtakes allow Close + EndPrint together while guaranteeing render progress.
        const WINDOW: usize = 32;
        const MAX_CLEANUP_OVERTAKES: usize = 2;
        if self.deferred.is_empty() { let request = self.received(receiver.recv()?); self.deferred.push_back(request); }
        if matches!(self.deferred.front(), Some(Request::Render(..)|Request::AdmittedRender(..))) && self.cleanup_overtakes < MAX_CLEANUP_OVERTAKES {
            let mut index = 1;
            while index < WINDOW {
                if index == self.deferred.len() {
                    match receiver.try_recv() {
                        Ok(request) => { let request = self.received(request); self.deferred.push_back(request) }
                        Err(_) => break,
                    }
                }
                match &self.deferred[index] {
                    Request::Render(..)|Request::AdmittedRender(..) => index += 1,
                    Request::Close(..) | Request::EndPrint(..) => {
                        self.cleanup_overtakes += 1;
                        return Ok(self.deferred.remove(index).expect("The cleanup index is buffered"));
                    }
                    _ => break,
                }
            }
        }
        self.cleanup_overtakes = 0;
        Ok(self.deferred.pop_front().expect("A request was received before dispatch"))
    }
}
#[derive(Clone)]
pub struct PdfService { sender: RequestSender }
static PDF_SERVICE: OnceLock<PdfService> = OnceLock::new();
type CacheKey = (u64, u16, i32);
struct Cache { entries: VecDeque<(CacheKey, Vec<u8>, usize)>, weight: usize }
impl Cache {
    fn get(&mut self, key: CacheKey) -> Option<Vec<u8>> {
        let i = self.entries.iter().position(|(k, _, _)| *k == key)?;
        let item = self.entries.remove(i)?;
        let bytes = item.1.clone(); self.entries.push_back(item); Some(bytes)
    }
    fn insert(&mut self, key: CacheKey, bytes: Vec<u8>, weight: usize) {
        const BUDGET: usize = 512 * 1024 * 1024;
        while self.weight + weight > BUDGET {
            if let Some((_, _, size)) = self.entries.pop_front() { self.weight -= size; } else { break; }
        }
        if weight <= BUDGET { self.weight += weight; self.entries.push_back((key, bytes, weight)); }
    }
    fn close(&mut self, id: u64) { self.entries.retain(|(key, _, _)| key.0 != id); self.weight = self.entries.iter().map(|(_, _, w)| w).sum(); }
}
impl PdfService {
    pub fn start(library: PathBuf) -> Self {
        PDF_SERVICE.get_or_init(|| Self::start_worker(library,if cfg!(test){4096}else{32},None)).clone()
    }
    pub fn start_with_recovery(library: PathBuf, recovery_root: PathBuf) -> Self {
        PDF_SERVICE.get_or_init(|| Self::start_worker(library,32,Some(recovery_root))).clone()
    }
    fn start_worker(library: PathBuf,render_limit:usize,recovery_root:Option<PathBuf>) -> Self {
        let (raw_sender, receiver) = mpsc::channel();
        let render_admission=Arc::new(Mutex::new(RenderAdmission::default()));
        let sender=RequestSender{inner:raw_sender,renders:render_admission.clone(),limit:render_limit,pending:Arc::default(),queue_limit:if cfg!(test){4096}else{WORKER_QUEUE_LIMIT}};
        let resource_cleanup = sender.clone();
        let queue_pending = sender.pending.clone();
        std::thread::Builder::new().name("pdf-worker".into()).spawn(move || {
            let pdfium = Pdfium::bind_to_library(library).map(Pdfium::new).map_err(|e| format!("PDF engine could not start: {e}"));
            let mut documents = HashMap::new();
            let mut sessions = HashMap::<u64, (EditSession, DocumentInfo)>::new();
            let mut note_documents = HashMap::new();
            let mut cache = Cache { entries: VecDeque::new(), weight: 0 };
            let mut page_labels = crate::page_labels::PageLabelCache::default();
            let mut next_id = 1;
            let mut pending = HashMap::<u64, PathBuf>::new();
            let mut recovery_offers = HashMap::<u64, crate::recovery_journal::RecoveryRecord>::new();
            let mut recovery_heads = HashMap::<u64, (crate::recovery_journal::SourceIdentity, u64)>::new();
            let mut recovery_floors = HashMap::<crate::recovery_journal::SourceIdentity, crate::recovery_journal::RecoveryRecord>::new();
            let mut recovery_locked = HashMap::<u64, RecoveryLock>::new();
            let mut recovery_pages = HashMap::<u64, u32>::new();
            let mut next_request = 1;
            let mut print_snapshots = HashMap::<u64, (std::rc::Rc<PdfDocument<'_>>, Vec<crate::editor::PageSpec>)>::new();
            let mut next_print = 1;
            #[cfg(test)]
            let mut render_work = HashMap::<u64, RenderWork>::new();
            #[cfg(test)]
            let mut bookmark_work = HashMap::<u64, BookmarkWork>::new();
            #[cfg(test)]
            let mut ocr_work = HashMap::<u64, OcrWork>::new();
            let mut requests = RequestQueue { pending: queue_pending, ..RequestQueue::default() };
            while let Ok(request) = requests.next(&receiver) {
                let request = match request {
                    Request::DurableEdit(id,edit,page,reply)=>{recovery_pages.insert(id,page);Request::Edit(id,edit,reply)},
                    Request::DurableEditAt(id,revision,edit,page,reply)=>{if sessions.get(&id).is_some_and(|(session,_)|session.revision!=revision){let _=reply.send(Err(STALE_PAGE_PLAN.into()));continue;}recovery_pages.insert(id,page);Request::Edit(id,edit,reply)},
                    Request::SaveAt(id,revision,pages,path,reply)=>{if sessions.get(&id).is_some_and(|(session,_)|session.revision!=revision){let _=reply.send(Err(STALE_PAGE_PLAN.into()));continue;}Request::Save(id,pages,path,reply)},
                    Request::DurableCrop(id,index,revision,rect,page,reply)=>{recovery_pages.insert(id,page);Request::Crop(id,index,revision,rect,reply)},
                    Request::DurableCropPages(id,pages,revision,insets,page,reply)=>{recovery_pages.insert(id,page);Request::CropPages(id,pages,revision,insets,reply)},
                    Request::DurableResetCrops(id,pages,revision,page,reply)=>{recovery_pages.insert(id,page);Request::ResetCrops(id,pages,revision,reply)},
                    Request::DurableComment(id,revision,mutation,page,reply)=>{recovery_pages.insert(id,page);Request::Comment(id,revision,mutation,reply)},
                    Request::Combine(first, second, path, reply) => Request::CreateCopy(first, second, CopyOperation::Combine, path, reply),
                    Request::InsertPages(target, donor, at, path, reply) => Request::CreateCopy(target, donor, CopyOperation::Insert { at }, path, reply),
                    Request::ReplacePages(target, donor, start, count, path, reply) => Request::CreateCopy(target, donor, CopyOperation::Replace { start, count }, path, reply),
                    Request::BeginOpen(path, reply) => {
                        if reply.is_closed() { continue; }
                        if pending.len() >= 8 { let _ = reply.send(Err("Close an existing password prompt before opening another PDF.".into())); continue; }
                        let request_id = next_request; next_request += 1;
                        pending.insert(request_id, path);
                        Request::Unlock(request_id, String::new(), reply)
                    }
                    other => other,
                };
                match request {
                    #[cfg(test)]
                    Request::BacklogGate(release, reply) => {
                        let _ = reply.send(Ok(()));
                        if let Ok(batch) = release.recv_timeout(std::time::Duration::from_secs(5)) {
                            for request in batch.into_iter().rev() { requests.deferred.push_front(request); }
                        }
                    }
                    #[cfg(test)]
                    Request::RenderWorkForDocument(id, reply) => { let _ = reply.send(Ok(render_work.remove(&id).unwrap_or_default())); }
                    #[cfg(test)]
                    Request::BookmarkWorkForDocument(id, reply) => { let _ = reply.send(Ok(bookmark_work.remove(&id).unwrap_or_default())); }
                    #[cfg(test)]
                    Request::OcrWorkForDocument(id, reply) => { let _ = reply.send(Ok(ocr_work.remove(&id).unwrap_or_default())); }
                    #[cfg(test)]
                    Request::OcrWorkerState(id, reply) => {
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let note_state = note_documents.get(&id).map(|(revision, _)| vec![(id, *revision)]).unwrap_or_default();
                            Ok(OcrWorkerState {
                                source: session.source.clone(), plan: session.plan.clone(), revision: session.revision,
                                dirty: session.dirty(), can_undo: session.can_undo(), can_redo: session.can_redo(),
                                cache_entries: cache.entries.iter().filter(|(key, _, _)| key.0 == id).map(|(key, bytes, _)| (*key, bytes.len())).collect(),
                                cache_weight: cache.entries.iter().filter(|(key, _, _)| key.0 == id).map(|(_, _, weight)| *weight).sum(),
                                note_documents: note_state,
                            })
                        })();
                        let _ = reply.send(result);
                    }
                    #[cfg(test)]
                    Request::OpenDocumentsForPath(path, reply) => {
                        let ids = sessions.iter().filter_map(|(id, (_, info))| (PathBuf::from(&info.path) == path).then_some(*id)).collect();
                        let _ = reply.send(Ok(ids));
                    }
                    #[cfg(test)]
                    Request::PasswordRequestsForPath(path, reply) => {
                        let ids = pending.iter().filter_map(|(id, source)| (*source == path).then_some(*id)).collect();
                        let _ = reply.send(Ok(ids));
                    }
                    #[cfg(test)]
                    Request::EnginePageLabels(id, reply) => {
                        let result = documents.get(&id).ok_or_else(|| "Document is closed".to_owned()).map(|document: &std::rc::Rc<PdfDocument<'_>>| document.pages().iter().map(|page| page.label().map(str::to_owned)).collect());
                        let _ = reply.send(result);
                    }
                    Request::BeginPrint(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            if print_snapshots.len() >= 4 { return Err("Wait for another print job to finish.".into()); }
                            let (session, info) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Start printing again.".into()); }
                            let document: &std::rc::Rc<PdfDocument<'_>> = documents.get(&id).ok_or("Document is closed")?;
                            if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) { return Err("Printing encrypted or restricted PDFs is not supported in this build.".into()); }
                            let token = next_print; next_print += 1;
                            let document = note_document(pdfium.as_ref().map_err(Clone::clone)?, document, session, &mut note_documents, id)?;
                            print_snapshots.insert(token, (document, session.plan.clone()));
                            Ok(PrintSnapshotInfo { token, pages: session.plan.len(), name: info.name.clone(), cleanup: resource_cleanup.clone() })
                        })();
                        if let Err(Ok(snapshot)) = reply.send(result) { print_snapshots.remove(&snapshot.token); }
                    }
                    Request::EndPrint(token, reply) => { print_snapshots.remove(&token); let _ = reply.send(Ok(())); }
                    Request::PrintRender(token, index, max_width, max_height, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (document, plan) = print_snapshots.get(&token).ok_or("Print job has ended")?;
                            let spec = plan.get(index).ok_or("Print page is out of range")?;
                            with_planned_page(document, spec, |page| {
                                let (width, height) = print_dimensions(page.width().value, page.height().value, max_width, max_height)?;
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_fixed_size(width as i32, height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE).render_annotations(true).render_form_data(true).use_print_quality(true)).map_err(|error| error.to_string())?;
                                let bgra = bitmap.as_raw_bytes();
                                if bitmap.width() != width as i32 || bitmap.height() != height as i32 || bgra.len() != width as usize * height as usize * 4 { return Err("Unexpected print bitmap layout.".into()); }
                                Ok(PrintBitmap { width, height, bgra })
                            })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::PreflightPageImage(id, revision, page, dpi, format, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, info) = sessions.get(&id).ok_or("Document is closed")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let _ = checked_page_image_request(session, document, revision, page, dpi, format)?;
                            let source_path = PathBuf::from(&info.path);
                            let stem = source_path.file_stem().filter(|stem| !stem.is_empty()).unwrap_or_default().to_string_lossy().into_owned();
                            let stem = if stem.is_empty() { "document".to_owned() } else { stem };
                            Ok(PageImagePreflight { suggested_name: format!("{stem}-page-{}.{}", usize::from(page) + 1, format.extension()) })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::ExportPageImage(id, revision, page, dpi, format, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let original = documents.get(&id).ok_or("Document is closed")?;
                            let (dimensions, spec) = checked_page_image_request(session, original, revision, page, dpi, format)?;
                            if reply.is_closed() { return Err(format!("{} export was canceled.", format.label())); }
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let mut export_note_documents = HashMap::new();
                            let document = note_document(engine, original, session, &mut export_note_documents, id)?;
                            let bgra = with_planned_page(&document, &spec, |page| {
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_fixed_size(dimensions.width as i32, dimensions.height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE).render_annotations(true).render_form_data(true).use_print_quality(true)).map_err(|error| format!("Could not render the {} page: {error}", format.label()))?;
                                let bgra = bitmap.as_raw_bytes();
                                let expected = usize::try_from(u64::from(dimensions.width) * u64::from(dimensions.height) * 4).map_err(|_| format!("The {} bitmap size is out of range.", format.label()))?;
                                if bitmap.width() != dimensions.width as i32 || bitmap.height() != dimensions.height as i32 || bitmap.format().map_err(|error| format!("Could not inspect the {} bitmap format: {error}", format.label()))? != PdfBitmapFormat::BGRA || bgra.len() != expected { return Err(format!("Unexpected {} export bitmap layout.", format.label())); }
                                Ok(bgra)
                            })?;
                            if reply.is_closed() { return Err(format!("{} export was canceled.", format.label())); }
                            crate::page_image::write_page_image(&path, dimensions, dpi, &bgra, format, || reply.is_closed())?;
                            Ok(PageImageReceipt { path: path.to_string_lossy().into_owned(), document_id: id, revision, page, dpi, width: dimensions.width, height: dimensions.height, format })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::CheckRevision(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let _ = reply.send(match sessions.get(&id) { None => Err("Document is closed".into()), Some((session, _)) if session.revision != revision => Err(STALE_PAGE_PLAN.into()), Some(_) => Ok(()) });
                    }
                    Request::PreflightRasterRedaction(id, revision, page, rectangles, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, info) = sessions.get(&id).ok_or("Document is closed")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let _ = checked_raster_redaction_request(session, document, revision, page, &rectangles)?;
                            let source_path = PathBuf::from(&info.path);
                            let stem = source_path.file_stem().filter(|stem| !stem.is_empty()).unwrap_or_default().to_string_lossy().into_owned();
                            let stem = if stem.is_empty() { "document".to_owned() } else { stem };
                            Ok(RasterRedactionPreflight { suggested_name: format!("{stem}-redacted.pdf") })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::RasterRedact(id, revision, page, rectangles, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let original = documents.get(&id).ok_or("Document is closed")?;
                            let plan = checked_raster_redaction_request(session, original, revision, page, &rectangles)?;
                            if reply.is_closed() { return Err("Redaction was canceled.".into()); }
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let mut redaction_note_documents = HashMap::new();
                            let document = note_document(engine, original, session, &mut redaction_note_documents, id)?;
                            let mut raster_pages = Vec::with_capacity(plan.pages.len());
                            for (index, raster) in plan.pages.iter().enumerate() {
                                if reply.is_closed() { return Err("Redaction was canceled.".into()); }
                                let bgra = with_planned_page(&document, &raster.spec, |page| {
                                    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_fixed_size(raster.dimensions.width as i32, raster.dimensions.height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE).render_annotations(true).render_form_data(true).use_print_quality(true)).map_err(|error| format!("Could not render redaction page {}: {error}", index + 1))?;
                                    let bgra = bitmap.as_raw_bytes();
                                    let expected = usize::try_from(u64::from(raster.dimensions.width) * u64::from(raster.dimensions.height) * 4).map_err(|_| "The redaction bitmap size is out of range.")?;
                                    if bitmap.width() != raster.dimensions.width as i32 || bitmap.height() != raster.dimensions.height as i32 || bitmap.format().map_err(|error| format!("Could not inspect the redaction bitmap: {error}"))? != PdfBitmapFormat::BGRA || bgra.len() != expected { return Err("Unexpected redaction bitmap layout.".into()); }
                                    Ok(bgra)
                                })?;
                                let rgb = crate::page_image::bgra_to_rgb(raster.dimensions, &bgra, PageImageFormat::Png)?;
                                raster_pages.push(crate::raster_redaction::RasterRedactionPage {
                                    page_width: raster.page_width,
                                    page_height: raster.page_height,
                                    image_width: raster.dimensions.width,
                                    image_height: raster.dimensions.height,
                                    rgb,
                                    rectangles: if index == usize::from(page) { rectangles.clone() } else { Vec::new() },
                                });
                            }
                            let prepared = crate::raster_redaction::redact_raster_pages(raster_pages)?;
                            crate::sanitization::verify_clean_raster_pdf(&prepared.bytes, plan.pages.len())?;
                            let check = engine.load_pdf_from_byte_vec(prepared.bytes.clone(), None).map_err(|error| format!("Redacted output validation failed: {error}"))?;
                            if check.pages().len() as usize != plan.pages.len() { return Err("Redacted output page count validation failed.".into()); }
                            for (index, raster) in plan.pages.iter().enumerate() {
                                let output_page = check.pages().get(index as i32).map_err(|error| format!("Could not reopen redacted page {}: {error}", index + 1))?;
                                let bitmap = output_page.render_with_config(&PdfRenderConfig::new().set_fixed_size(raster.dimensions.width as i32, raster.dimensions.height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE)).map_err(|error| format!("Could not render reopened redacted page {}: {error}", index + 1))?;
                                if bitmap.width() != raster.dimensions.width as i32 || bitmap.height() != raster.dimensions.height as i32 { return Err("Reopened redaction render dimensions differ from the plan.".into()); }
                            }
                            let parsed = lopdf::Document::load_mem(&prepared.bytes).map_err(|error| format!("Could not inspect redacted output text: {error}"))?;
                            for page_number in 1..=plan.pages.len() as u32 {
                                if !parsed.extract_text(&[page_number]).map_err(|error| format!("Could not inspect redacted page text: {error}"))?.is_empty() { return Err("Redacted output unexpectedly retains extractable text.".into()); }
                            }
                            if reply.is_closed() { return Err("Redaction was canceled.".into()); }
                            write_new_file(&path, &prepared.bytes)?;
                            let pages = page_sizes(&check)?;
                            let new_id = next_id; next_id += 1;
                            let info = DocumentInfo { id: new_id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(new_id, (EditSession::new(prepared.bytes, info.pages.len()), info.clone()));
                            documents.insert(new_id, std::rc::Rc::new(check));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let cleanup = UnclaimedReply::Document(saved.document.id); ReplyLease::new(saved, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    #[cfg(windows)]
                    Request::PreflightSearchableOcr(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, info) = sessions.get(&id).ok_or("Document is closed")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let page_count = checked_searchable_ocr_request(session, document, revision)?;
                            let source = PathBuf::from(&info.path);
                            let stem = source.file_stem().filter(|stem| !stem.is_empty()).unwrap_or_default().to_string_lossy();
                            Ok(SearchableOcrPreflight { suggested_name: format!("{}-searchable.pdf", if stem.is_empty() { "document" } else { &stem }), page_count })
                        })();
                        let _ = reply.send(result);
                    }
                    #[cfg(windows)]
                    Request::PublishSearchableOcr(id, revision, path, prepared, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let ensure_active = || -> Result<(), String> {
                                prepared.cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                                if std::time::Instant::now() >= prepared.deadline { return Err("OCR exceeded its time limit.".into()); }
                                Ok(())
                            };
                            ensure_active()?;
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let original = documents.get(&id).ok_or("Document is closed")?;
                            let page_count = checked_searchable_ocr_request(session, original, revision)?;
                            if page_count != prepared.pages.len() || prepared.expected_text.len() != page_count { return Err("Searchable OCR output no longer matches the source document.".into()); }
                            crate::searchable_pdf::verify_searchable_pdf(&prepared.bytes, &prepared.pages)?;
                            ensure_active()?;
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let check = engine.load_pdf_from_byte_vec(prepared.bytes.clone(), None).map_err(|error| format!("Searchable OCR output validation failed: {error}"))?;
                            ensure_active()?;
                            if check.pages().len() as usize != page_count { return Err("Searchable OCR output page count validation failed.".into()); }
                            for (index, expected) in prepared.pages.iter().enumerate() {
                                if std::time::Instant::now() >= prepared.deadline { return Err("OCR exceeded its time limit.".into()); }
                                if reply.is_closed() { return Err("OCR was cancelled.".into()); }
                                let page = check.pages().get(index as i32).map_err(|error| format!("Could not reopen searchable OCR page {}: {error}", index + 1))?;
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_fixed_size(expected.image_width as i32, expected.image_height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE)).map_err(|error| format!("Could not render searchable OCR page {}: {error}", index + 1))?;
                                ensure_active()?;
                                let bgra = bitmap.as_raw_bytes();
                                let expected_bgra = expected.rgb.len().checked_div(3).and_then(|pixels| pixels.checked_mul(4)).ok_or("Searchable OCR render length overflows.")?;
                                if bgra.len() != expected_bgra || bgra.chunks_exact(4).any(|pixel| pixel[3] != 255) { return Err(format!("Searchable OCR page {} has an unexpected render layout.", index + 1)); }
                                let rgb = bgra.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0]]).collect::<Vec<_>>();
                                if rgb != expected.rgb { return Err(format!("Searchable OCR page {} pixels changed after reopening.", index + 1)); }
                                let text = page.text().map_err(|error| format!("Could not extract searchable OCR page {} text: {error}", index + 1))?;
                                ensure_active()?;
                                let visible = page.boundaries().bounding().map_err(|error| error.to_string())?.bounds;
                                if text.inside_rect(visible) != prepared.expected_text[index] { return Err(format!("Searchable OCR page {} extraction differs from the OCR text.", index + 1)); }
                            }
                            ensure_active()?;
                            if reply.is_closed() { return Err("OCR was cancelled.".into()); }
                            publish_searchable_file(&path, &prepared.bytes, prepared.deadline, &prepared.cancellation, None, None)?;
                            let pages = page_sizes(&check)?;
                            let new_id = next_id; next_id += 1;
                            let info = DocumentInfo { id: new_id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(new_id, (EditSession::new(prepared.bytes, info.pages.len()), info.clone()));
                            documents.insert(new_id, std::rc::Rc::new(check));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let id = saved.document.id; ReplyLease::new(saved, UnclaimedReply::Document(id), resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    #[cfg(windows)]
                    Request::OcrRaster(request, cancellation, admission, reply) => {
                        #[cfg(test)]
                        { ocr_work.entry(request.document_id).or_default().dequeued += 1; }
                        if reply.is_closed() || cancellation.is_cancelled() { continue; }
                        let result = (|| {
                            cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                            let (session, _) = sessions.get(&request.document_id).ok_or("Document is closed")?;
                            let original = documents.get(&request.document_id).ok_or("Document is closed")?;
                            session.text_extraction_permission()?;
                            let (dimensions, spec, page_width, page_height) = checked_ocr_request(session, original, request)?;
                            #[cfg(test)]
                            { ocr_work.entry(request.document_id).or_default().preflighted += 1; }
                            cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                            if reply.is_closed() { return Err("OCR was cancelled".into()); }
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let mut ocr_note_documents = HashMap::new();
                            let document = note_document(engine, original, session, &mut ocr_note_documents, request.document_id)?;
                            cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                            let bgra = with_planned_page(&document, &spec, |page| {
                                if cancellation.is_cancelled() || reply.is_closed() { return Err("OCR was cancelled".into()); }
                                #[cfg(test)]
                                { ocr_work.entry(request.document_id).or_default().rendered += 1; }
                                let bitmap = page.render_with_config(&PdfRenderConfig::new()
                                    .set_fixed_size(dimensions.width as i32, dimensions.height as i32)
                                    .set_format(PdfBitmapFormat::BGRA)
                                    .set_reverse_byte_order(false)
                                    .clear_before_rendering(true)
                                    .set_clear_color(PdfColor::WHITE)
                                    .render_annotations(true)
                                    .render_form_data(true)
                                    .use_print_quality(true))
                                    .map_err(|error| format!("Could not render the OCR page: {error}"))?;
                                let bgra = bitmap.as_raw_bytes();
                                if bitmap.width() != dimensions.width as i32
                                    || bitmap.height() != dimensions.height as i32
                                    || bitmap.format().map_err(|error| format!("Could not inspect the OCR bitmap format: {error}"))? != PdfBitmapFormat::BGRA
                                    || bgra.len() != dimensions.bgra_bytes
                                {
                                    return Err("Unexpected OCR bitmap layout.".into());
                                }
                                drop(bitmap);
                                Ok(bgra)
                            })?;
                            drop(document);
                            drop(ocr_note_documents);
                            cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                            if reply.is_closed() { return Err("OCR was cancelled".into()); }
                            let p6 = crate::ocr::bgra_to_p6(dimensions, &bgra, &cancellation)?;
                            #[cfg(test)]
                            { ocr_work.entry(request.document_id).or_default().converted += 1; }
                            cancellation.ensure_runnable().map_err(|error| error.to_string())?;
                            if reply.is_closed() { return Err("OCR was cancelled".into()); }
                            Ok(crate::ocr::OcrPageRaster {
                                request,
                                width: dimensions.width,
                                height: dimensions.height,
                                page_width,
                                page_height,
                                p6,
                            })
                        })();
                        drop(admission);
                        if !reply.is_closed() { let _ = reply.send(result); }
                    }
                    #[cfg(windows)]
                    Request::ValidateOcrResult(request, admission, reply) => {
                        if reply.is_closed() { continue; }
                        let result = validate_ocr_result(&sessions, &documents, request);
                        drop(admission);
                        let _ = reply.send(result);
                    }
                    Request::BeginOpen(_, _)|Request::DurableEdit(..)|Request::DurableEditAt(..)|Request::SaveAt(..)|Request::DurableCrop(..)|Request::DurableCropPages(..)|Request::DurableResetCrops(..)|Request::DurableComment(..) => unreachable!(),
                    Request::CancelPassword(id, reply) => { pending.remove(&id); let _ = reply.send(Ok(())); }
                    Request::Unlock(request_id, password, reply) => {
                        if reply.is_closed() { pending.remove(&request_id); continue; }
                        let result = (|| {
                            let path = pending.get(&request_id).ok_or("Password request expired. Open the PDF again.")?.clone();
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            if password.contains('\0') { return Err("Passwords cannot contain a null character.".into()); }
                            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                            let document = match engine.load_pdf_from_byte_vec(bytes.clone(), Some(&password)) {
                                Ok(document) => document,
                                Err(PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)) => return Ok(OpenResult::PasswordRequired { request_id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), incorrect: !password.is_empty() }),
                                Err(error) => return Err(format!("Unable to open PDF: {error}")),
                            };
                            let pages = page_sizes(&document)?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            let recovery = if matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) {
                                recovery_root.as_deref().map(|root| {
                                    let source = recovery_source_identity(&bytes)?;
                                    Ok::<_, String>(crate::recovery_store::read_recovery(root, source, None, 0)?.filter(|record| record.active))
                                }).transpose()?.flatten()
                            } else { None };
                            sessions.insert(id, (EditSession::new_with_password(bytes, info.pages.len(), &password), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document));
                            let offer = recovery.as_ref().map(|record| RecoveryOffer { revision: record.revision, current_page: record.current_page });
                            if let Some(record) = recovery { recovery_offers.insert(id, record); }
                            Ok(OpenResult::Opened { document: info, recovery: offer })
                        })();
                        if !matches!(&result, Ok(OpenResult::PasswordRequired { .. })) { pending.remove(&request_id); }
                        let result = result.map(|value| {
                            let cleanup = match &value { OpenResult::Opened { document, .. } => UnclaimedReply::Document(document.id), OpenResult::PasswordRequired { request_id, .. } => UnclaimedReply::Password(*request_id) };
                            ReplyLease::new(value, cleanup, resource_cleanup.clone())
                        });
                        if let Err(result) = reply.send(result) {
                            pending.remove(&request_id);
                            if let Ok(value) = &result { if let OpenResult::Opened { document, .. } = &**value { documents.remove(&document.id); sessions.remove(&document.id); } }
                        }
                    }
                    Request::Open(path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                            let document = engine.load_pdf_from_byte_vec(bytes.clone(), None).map_err(|e| format!("Unable to open PDF: {e}"))?;
                            let pages = page_sizes(&document)?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(id, (EditSession::new(bytes, info.pages.len()), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document)); Ok(info)
                        })();
                        let result = result.map(|info| { let cleanup = UnclaimedReply::Document(info.id); ReplyLease::new(info, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(info)) = reply.send(result) { documents.remove(&info.id); sessions.remove(&info.id); }
                    },
                    Request::CheckpointRecovery(root, id, revision, current_page, reply) => {
                        if reply.is_closed() { continue; }
                        let mut result = (|| {
                            let (session, original) = sessions.get(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before updating recovery.")); }
                            if session.revision != revision { return Err("Document changed. Save recovery again.".into()); }
                            if !session.dirty() { return Err("Only unsaved edits can be stored for recovery.".into()); }
                            if current_page as usize >= session.plan.len() { return Err("Current page is out of range.".into()); }
                            ensure_recovery_owner(Some(&root), &recovery_heads, &mut recovery_floors, id, session)?;
                            if recovery_checkpoint_current(&root, recovery_heads.get(&id), session, original, current_page)? { return Ok(()); }
                            let record = persist_recovery_state(Some(&root), &mut recovery_floors, session, original, current_page)?.ok_or("Recovery is unavailable.")?;
                            recovery_heads.insert(id, (record.source, record.generation));
                            Ok(())
                        })();
                        if matches!(&result, Err(error) if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED)) {
                            if let Some((session, _)) = sessions.get(&id) { if let Some(lock) = record_recovery_lock(Some(&root), &mut recovery_locked, id, revision, session) { result=Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision)); } }
                        }
                        let _ = reply.send(result);
                    }
                    Request::RestoreRecovery(root, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                            let source = recovery_source_identity(&bytes)?;
                            let Some(record) = crate::recovery_store::read_recovery(&root, source, None, 1)? else { return Ok(None); };
                            if !record.active { return Ok(None); }
                            accept_recovery_record(&mut recovery_floors, &record)?;
                            if recovery_heads.values().any(|(identity, _)| *identity == source) { return Err("Another open tab already owns recovery for this PDF.".into()); }
                            let document = engine.load_pdf_from_byte_vec(bytes.clone(), None).map_err(|error| format!("Unable to open PDF: {error}"))?;
                            if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) { return Err("Encrypted or restricted PDFs cannot be restored from recovery.".into()); }
                            let pages = page_sizes(&document)?;
                            let mut session = EditSession::new(bytes, pages.len());
                            restore_recovery_session(&mut session, &record)?;
                            session.export(None)?;
                            let id = next_id; next_id += 1;
                            let original = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            let info = current_info(&session, &original, &document)?;
                            sessions.insert(id, (session, original));
                            documents.insert(id, std::rc::Rc::new(document));
                            recovery_offers.retain(|_, offer| offer.source != source);
                            recovery_heads.insert(id, (source, record.generation));
                            let recovered = RecoveredDocument { document: info, current_page: record.current_page };
                            Ok(Some(ReplyLease::new(recovered, UnclaimedReply::Document(id), resource_cleanup.clone())))
                        })();
                        if let Err(Ok(Some(lease))) = reply.send(result) {
                            let id = lease.document.id; documents.remove(&id); sessions.remove(&id);
                        }
                    }
                    Request::KeepRecoveredEdits(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let record = recovery_offers.get(&id).ok_or("Recovery choice is no longer available.")?.clone();
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if session.revision != revision || revision != 0 { return Err("Document changed. Reopen it to choose recovery.".into()); }
                            if recovery_heads.iter().any(|(owner,(source,_))| *owner!=id && *source==record.source) { return Err("Another open tab already owns recovery for this PDF.".into()); }
                            let root=recovery_root.as_deref().ok_or("Recovery is unavailable.")?;let current=crate::recovery_store::read_recovery(root,record.source,None,0)?.ok_or("Recovery choice is no longer available.")?;
                            if current!=record || !current.active { return Err("Recovery choice is stale. Reopen the PDF.".into()); }
                            let snapshot = session.snapshot();
                            if let Err(error) = restore_recovery_session(session, &record).and_then(|_| session.export(None).map(|_| ())) {
                                session.restore(snapshot); return Err(error);
                            }
                            let info = match documents.get(&id).ok_or_else(||"Document is closed".to_owned()).and_then(|document| current_info(session, original, document)) { Ok(info) => info, Err(error) => { session.restore(snapshot); return Err(error); } };
                            recovery_offers.retain(|_,offer|offer.source!=record.source);
                            recovery_heads.insert(id, (record.source, record.generation));
                            Ok(RecoveredDocument { document: info, current_page: record.current_page })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::OpenOriginal(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let record = recovery_offers.get(&id).ok_or("Recovery choice is no longer available.")?.clone();
                            let (session, original) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision || revision != 0 { return Err("Document changed. Reopen it to choose recovery.".into()); }
                            let root = recovery_root.as_deref().ok_or("Recovery is unavailable.")?;
                            let current=crate::recovery_store::read_recovery(root,record.source,None,0)?.ok_or("Recovery choice is no longer available.")?;
                            if current!=record || !current.active { return Err("Recovery choice is stale. Reopen the PDF.".into()); }
                            let tombstone = crate::recovery_journal::RecoveryRecord { active: false, generation: record.generation.checked_add(1).ok_or("Recovery generation is exhausted.")?, revision: record.revision, current_page: 0, source_pages: record.source_pages, source: record.source, edit_state: crate::recovery_journal::RecoveryEditState { pages: Vec::new() } };
                            if let Err(error)=write_recovery_verified(root, &tombstone){if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{revision}:{}",tombstone.revision));}return Err(error);}
                            accept_recovery_record(&mut recovery_floors, &tombstone)?;
                            recovery_offers.retain(|_,offer|offer.source!=record.source);
                            current_info(session, original, documents.get(&id).ok_or("Document is closed")?)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::DiscardRecovery(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get(&id).ok_or("Document is closed")?;
                            if let Some(lock) = recovery_locked.get(&id).cloned() {
                                if revision != lock.requested_revision { return Err(format!("Recovery discard is stale ({revision} != {}).", lock.requested_revision)); }
                                let root = recovery_root.as_deref().ok_or("Recovery is unavailable.")?;
                                let current = crate::recovery_store::read_recovery(root, lock.source, None, 0)?.ok_or("Recovery edits are no longer available.")?;
                                if !current.active {
                                    if current.revision == lock.committed_revision && current.generation >= lock.generation { return Ok(()); }
                                    return Err("Recovery discard is stale.".into());
                                }
                                if current.source != lock.source || current.revision != lock.committed_revision || current.generation != lock.generation { return Err(format!("Recovery discard is stale (record {}:{} != {}:{}).", current.revision, current.generation, lock.committed_revision, lock.generation)); }
                                let tombstone = crate::recovery_journal::RecoveryRecord { active: false, generation: current.generation.checked_add(1).ok_or("Recovery generation is exhausted.")?, revision: lock.committed_revision, current_page: 0, source_pages: current.source_pages, source: current.source, edit_state: crate::recovery_journal::RecoveryEditState { pages: Vec::new() } };
                                if let Err(error)=write_recovery_verified(root, &tombstone){if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));}return Err(error);}
                                accept_recovery_record(&mut recovery_floors, &tombstone)?;
                                recovery_heads.remove(&id);
                                return Ok(());
                            }
                            if session.revision != revision { return Err("Document changed. Discard again.".into()); }
                            if !session.dirty() { return Err("Only unsaved edits can be discarded.".into()); }
                            let root = recovery_root.as_deref().ok_or("Recovery is unavailable.")?;
                            let source = recovery_source_identity(&session.source)?;
                            if recovery_heads.iter().any(|(owner, (identity, _))| *owner != id && *identity == source) { return Err("Another open tab owns recovery for this PDF.".into()); }
                            let current = crate::recovery_store::read_recovery(root, source, None, 0)?.ok_or("Recovery edits are no longer available.")?;
                            accept_recovery_record(&mut recovery_floors, &current)?;
                            if !current.active {
                                if current.revision != revision { return Err("Recovery discard is stale.".into()); }
                                return Ok(());
                            }
                            let disk = std::fs::read(&original.path).map_err(|_| "The source PDF changed or became unavailable; recovery was not discarded.".to_owned())?;
                            if disk != session.source { return Err("The source PDF changed outside the app; recovery was not discarded.".into()); }
                            if recovery_heads.get(&id) != Some(&(source, current.generation)) { return Err("Recovery edits are owned by another session.".into()); }
                            let tombstone = crate::recovery_journal::RecoveryRecord { active: false, generation: current.generation.checked_add(1).ok_or("Recovery generation is exhausted.")?, revision, current_page: 0, source_pages: u32::try_from(original.pages.len()).map_err(|_| "Recovery source has too many pages.")?, source, edit_state: crate::recovery_journal::RecoveryEditState { pages: Vec::new() } };
                            if let Err(error)=write_recovery_verified(root, &tombstone){if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{revision}:{revision}"));}return Err(error);}
                            accept_recovery_record(&mut recovery_floors, &tombstone)?;
                            recovery_heads.remove(&id);
                            recovery_offers.retain(|_, offer| offer.source != source);
                            Ok(())
                        })();
                        let _ = reply.send(result);
                    }
                    Request::AdmittedRender(id,page,width,token)=>{let reply={let mut a=render_admission.lock().expect("render admission mutex poisoned");let key=a.keys.iter().find_map(|(key,value)|(*value==token).then_some(*key));if let Some(key)=key{a.keys.remove(&key);}a.replies.remove(&token)};let Some(reply)=reply else{continue};
                        #[cfg(test)]
                        { render_work.entry(id).or_default().dequeued += 1; }
                        if reply.is_closed() {
                            #[cfg(test)]
                            { render_work.entry(id).or_default().skipped_closed += 1; }
                            continue;
                        }
                        let width = width.clamp(64, 3000);
                        let key = (id, page, width);
                        let result = if let Some(bytes) = cache.get(key) { Ok(bytes) } else {
                            (|| {
                                let original = documents.get(&id).ok_or("Document is closed")?;
                                let session = &sessions.get(&id).ok_or("Document is closed")?.0;
                                let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                                let document = note_document(pdfium.as_ref().map_err(Clone::clone)?, original, session, &mut note_documents, id)?;
                                let image = with_planned_page(&document, spec, |page| {
                                    #[cfg(test)]
                                    { render_work.entry(id).or_default().rendered += 1; }
                                    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(width).set_maximum_height(5000).render_annotations(true)).map_err(|e| e.to_string())?;
                                    bitmap.as_image().map_err(|e| e.to_string())
                                })?;
                                let mut bytes = Cursor::new(Vec::new());
                                image.write_to(&mut bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?;
                                let bytes = bytes.into_inner();
                                let weight = bytes.len() + image.width() as usize * image.height() as usize * 4;
                                cache.insert(key, bytes.clone(), weight); Ok(bytes)
                            })()
                        };
                        let _ = reply.send(result);
                    },
                    Request::Render(..)=>unreachable!("viewer renders are admitted before channel insertion"),
                    Request::Properties(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Reopen document properties.".into()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let dimensions = current_info(session, original, document)?.pages.iter().map(|page| (page.width, page.height)).collect::<Vec<_>>();
                            crate::document_properties::inspect(document, session.source.len(), &dimensions)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::PageLabels(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Reopen page labels.".into()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let source = page_labels.read(id, &session.source, document.pages().len() as usize);
                            Ok(crate::page_labels::project(&source, id, revision, &session.plan))
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Bookmarks(id, revision, reply) => {
                        #[cfg(test)]
                        { bookmark_work.entry(id).or_default().dequeued += 1; }
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Reopen bookmarks.".into()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let mut items = Vec::new();
                            let mut depths = HashMap::new();
                            let mut truncated = false;
                            let mut title_bytes = 0usize;
                            for bookmark in document.bookmarks().iter().take(1001) {
                                if reply.is_closed() { return Err("Bookmark reading was cancelled.".into()); }
                                if items.len() == 1000 { truncated = true; break; }
                                #[cfg(test)]
                                { bookmark_work.entry(id).or_default().visited += 1; }
                                let depth = bookmark.parent().and_then(|parent| depths.get(&parent).copied()).map_or(0, |depth: usize| depth + 1);
                                depths.insert(bookmark.clone(), depth);
                                let source = bookmark.destination().and_then(|destination| destination.page_index().ok());
                                let page = source.and_then(|source| session.plan.iter().position(|spec| spec.source == source as usize));
                                let title = bookmark.title().filter(|s| !s.is_empty()).unwrap_or_else(|| "Untitled bookmark".into());
                                title_bytes = checked_read_text_bytes(title_bytes, title.len(), "Bookmarks contain too much text.")?;
                                items.push(BookmarkInfo { title, page, depth });
                            }
                            Ok(BookmarkList { items, truncated })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::TextGeometry(id, index, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Select text again.".into()); }
                            let spec = session.plan.get(index as usize).ok_or("Page is out of range")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            session.text_extraction_permission()?;
                            with_planned_page(document, spec, |page| crate::text_geometry::inspect(page, id, index, revision))
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Text(id, page, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Search again.".into()); }
                            let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            session.text_extraction_permission()?;
                            with_planned_page(document, spec, |source| {
                                let text = source.text().map_err(|e| e.to_string())?;
                                let visible = source.boundaries().bounding().map_err(|e| e.to_string())?.bounds;
                                let visible = text.inside_rect(visible);
                                checked_read_text_bytes(0, visible.len(), "This page has too much embedded text to read or search.")?;
                                Ok(visible)
                            })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Edit(id, edit, reply) => {
                        let durable_page=recovery_pages.remove(&id);
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before making more edits.")); }
                            if recovery_offers.contains_key(&id) { return Err("Choose whether to keep the recovered edits before editing this PDF.".into()); }
                            if durable_page.is_some_and(|page|page as usize>=session.plan.len()){return Err("Current page is out of range.".into());}
                            ensure_recovery_owner(recovery_root.as_deref(),&recovery_heads,&mut recovery_floors,id,session)?;
                            let requested_revision = session.revision;
                            let snapshot = session.snapshot();
                            session.apply(edit)?;
                            let info=match documents.get(&id).ok_or_else(||"Document is closed".to_owned()).and_then(|document|current_info(session,original,document)){Ok(info)=>info,Err(error)=>{session.restore(snapshot);return Err(error);}};
                            let current_page=durable_page.map_or(0,|page|page.min(session.plan.len().saturating_sub(1) as u32));match persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, current_page) { Ok(Some(record)) if record.active => { recovery_heads.insert(id,(record.source,record.generation)); }, Ok(Some(_)) => { recovery_heads.remove(&id); }, Ok(None) => {}, Err(error) => { if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED) { if let Some(lock)=record_recovery_lock(recovery_root.as_deref(), &mut recovery_locked, id, requested_revision, session){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));} } else { session.restore(snapshot); } return Err(error); } }
                            note_documents.remove(&id);
                            cache.close(id);
                            Ok(info)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Crop(id, index, revision, rect, reply) => {
                        let durable_page=recovery_pages.remove(&id);
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before making more edits.")); }
                            if recovery_offers.contains_key(&id) { return Err("Choose whether to keep the recovered edits before editing this PDF.".into()); }
                            if durable_page.is_some_and(|page|page as usize>=session.plan.len()){return Err("Current page is out of range.".into());}
                            ensure_recovery_owner(recovery_root.as_deref(),&recovery_heads,&mut recovery_floors,id,session)?;
                            let snapshot = session.snapshot();
                            if session.revision != revision { return Err("Document changed. Open the crop tool again.".into()); }
                            let spec = session.plan.get(index as usize).ok_or("Page is out of range")?;
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let crop = checked_displayed_crop(document, spec, rect)?;
                            session.apply(PageEdit::Crop { page: index as usize, crop })?;
                            let info=match current_info(session,original,document){Ok(info)=>info,Err(error)=>{session.restore(snapshot);return Err(error);}};
                            let current_page=durable_page.unwrap_or(u32::from(index)).min(session.plan.len().saturating_sub(1) as u32);match persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, current_page) { Ok(Some(record)) if record.active => { recovery_heads.insert(id,(record.source,record.generation)); }, Ok(Some(_)) => { recovery_heads.remove(&id); }, Ok(None) => {}, Err(error) => { if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED) { if let Some(lock)=record_recovery_lock(recovery_root.as_deref(), &mut recovery_locked, id, revision, session){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));} } else { session.restore(snapshot); } return Err(error); } }
                            note_documents.remove(&id);
                            cache.close(id);
                            Ok(info)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::CropPages(id, pages, revision, insets, reply) => {
                        let durable_page=recovery_pages.remove(&id);
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            validate_crop_insets(insets)?;
                            if pages.is_empty() || pages.windows(2).any(|pair| pair[0] >= pair[1]) {
                                return Err("Select one or more pages in ascending order without duplicates.".into());
                            }
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before making more edits.")); }
                            if recovery_offers.contains_key(&id) { return Err("Choose whether to keep the recovered edits before editing this PDF.".into()); }
                            if durable_page.is_some_and(|page|page as usize>=session.plan.len()){return Err("Current page is out of range.".into());}
                            ensure_recovery_owner(recovery_root.as_deref(),&recovery_heads,&mut recovery_floors,id,session)?;
                            let snapshot = session.snapshot();
                            if session.revision != revision { return Err("Document changed. Open the crop tool again.".into()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let mut crops = Vec::with_capacity(pages.len());
                            for page in pages {
                                let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                                let rect = displayed_inset_rect(spec, document, insets)?;
                                crops.push((page as usize, checked_displayed_crop(document, spec, rect)?));
                            }
                            session.apply(PageEdit::CropMany { crops })?;
                            let info=match current_info(session,original,document){Ok(info)=>info,Err(error)=>{session.restore(snapshot);return Err(error);}};
                            let current_page=durable_page.map_or(0,|page|page.min(session.plan.len().saturating_sub(1) as u32));match persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, current_page) { Ok(Some(record)) if record.active => { recovery_heads.insert(id,(record.source,record.generation)); }, Ok(Some(_)) => { recovery_heads.remove(&id); }, Ok(None) => {}, Err(error) => { if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED) { if let Some(lock)=record_recovery_lock(recovery_root.as_deref(), &mut recovery_locked, id, revision, session){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));} } else { session.restore(snapshot); } return Err(error); } }
                            note_documents.remove(&id);
                            cache.close(id);
                            Ok(info)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::ResetCrops(id, pages, revision, reply) => {
                        let durable_page=recovery_pages.remove(&id);
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            if pages.is_empty() || pages.windows(2).any(|pair| pair[0] >= pair[1]) {
                                return Err("Select one or more pages in ascending order without duplicates.".into());
                            }
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before making more edits.")); }
                            if recovery_offers.contains_key(&id) { return Err("Choose whether to keep the recovered edits before editing this PDF.".into()); }
                            if durable_page.is_some_and(|page|page as usize>=session.plan.len()){return Err("Current page is out of range.".into());}
                            ensure_recovery_owner(recovery_root.as_deref(),&recovery_heads,&mut recovery_floors,id,session)?;
                            let snapshot = session.snapshot();
                            if session.revision != revision { return Err("Document changed. Select pages again.".into()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let before = session.revision;
                            session.apply(PageEdit::ResetCropMany { pages: pages.into_iter().map(usize::from).collect() })?;
                            let info=match current_info(session,original,document){Ok(info)=>info,Err(error)=>{session.restore(snapshot);return Err(error);}};
                            let current_page=durable_page.map_or(0,|page|page.min(session.plan.len().saturating_sub(1) as u32));match persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, current_page) { Ok(Some(record)) if record.active => { recovery_heads.insert(id,(record.source,record.generation)); }, Ok(Some(_)) => { recovery_heads.remove(&id); }, Ok(None) => {}, Err(error) => { if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED) { if let Some(lock)=record_recovery_lock(recovery_root.as_deref(), &mut recovery_locked, id, revision, session){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));} } else { session.restore(snapshot); } return Err(error); } }
                            if session.revision != before {
                                note_documents.remove(&id);
                                cache.close(id);
                            }
                            Ok(info)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Comments(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let session = &sessions.get(&id).ok_or("Document is closed")?.0;
                            if session.revision != revision { return Err("Document changed. Refresh comments.".into()); }
                            let mut result = crate::comments::CommentList { document_id: id, revision, status: "supported", reason: None, notes: Vec::new() };
                            if let Some(reason) = session.comments_reason() { result.status = "unsupported"; result.reason = Some(reason.to_owned()); return Ok(result); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            for (index, spec) in session.plan.iter().enumerate() {
                                for note in spec.notes.iter().filter(|note| note.kind == crate::comments::AnnotationKind::Note) {
                                    let rect = with_planned_page(document, spec, |page| displayed_note_rect(page, note.rect))?;
                                    result.notes.push(crate::comments::CommentInfo { id: note.id.clone(), page: index, rect, contents: note.contents.clone() });
                                }
                            }
                            Ok(result)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Annotations(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let session = &sessions.get(&id).ok_or("Document is closed")?.0;
                            if session.revision != revision { return Err("Document changed. Refresh annotations.".into()); }
                            let mut result = crate::comments::AnnotationList { document_id: id, revision, status: "supported", reason: None, annotations: Vec::new() };
                            if let Some(reason) = session.comments_reason() { result.status = "unsupported"; result.reason = Some(reason.to_owned()); return Ok(result); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            for (index, spec) in session.plan.iter().enumerate() {
                                for note in &spec.notes {
                                    let (rect, quads) = with_planned_page(document, spec, |page| {
                                        if note.kind == crate::comments::AnnotationKind::Note { return Ok((displayed_note_rect(page, note.rect)?, None)); }
                                        let source = note.quads.as_deref().unwrap_or(std::slice::from_ref(&note.rect));
                                        let quads = source.iter().map(|bounds| displayed_note_rect(page, *bounds)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect::<Vec<_>>();
                                        let rect = quads.first().map(|first| {
                                            let mut bounds = (first.x, first.y, first.x + first.width, first.y + first.height);
                                            for quad in &quads { bounds.0 = bounds.0.min(quad.x); bounds.1 = bounds.1.min(quad.y); bounds.2 = bounds.2.max(quad.x + quad.width); bounds.3 = bounds.3.max(quad.y + quad.height); }
                                            crate::comments::DisplayRect { x: bounds.0, y: bounds.1, width: bounds.2 - bounds.0, height: bounds.3 - bounds.1 }
                                        });
                                        Ok((rect, Some(quads)))
                                    })?;
                                    result.annotations.push(crate::comments::AnnotationInfo { id: note.id.clone(), kind: note.kind, page: index, rect, contents: (!note.contents.is_empty()).then(|| note.contents.clone()), quads });
                                }
                            }
                            Ok(result)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Comment(id, revision, mutation, reply) => {
                        let durable_page=recovery_pages.remove(&id);
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if recovery_locked.contains_key(&id) { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Close and reopen this PDF before making more edits.")); }
                            if recovery_offers.contains_key(&id) { return Err("Choose whether to keep the recovered edits before editing this PDF.".into()); }
                            if durable_page.is_some_and(|page|page as usize>=session.plan.len()){return Err("Current page is out of range.".into());}
                            ensure_recovery_owner(recovery_root.as_deref(),&recovery_heads,&mut recovery_floors,id,session)?;
                            let snapshot = session.snapshot();
                            if session.revision != revision { return Err("Document changed. Refresh comments.".into()); }
                            if let Some(reason) = session.comments_reason() { return Err(reason.to_owned()); }
                            let document = documents.get(&id).ok_or("Document is closed")?;
                            let next = match mutation {
                                CommentMutation::Create(page, rect, contents) => {
                                    let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                                    let rect = with_planned_page(document, spec, |page| displayed_crop(page, rect))?;
                                    session.proposed_comment(Some((page as usize, rect)), None, Some(&contents))?
                                }
                                CommentMutation::Update(note_id, contents) => session.proposed_comment(None, Some(&note_id), Some(&contents))?,
                                CommentMutation::Delete(note_id) => session.proposed_comment(None, Some(&note_id), None)?,
                                CommentMutation::CreateHighlight(page, rect, contents) => {
                                    let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                                    let rect = with_planned_page(document, spec, |page| displayed_crop(page, rect))?;
                                    session.proposed_highlight(Some((page as usize, rect)), None, contents.as_deref(), false)?
                                }
                                CommentMutation::UpdateHighlight(id, contents) => session.proposed_highlight(None, Some(&id), contents.as_deref(), false)?,
                                CommentMutation::DeleteHighlight(id) => session.proposed_highlight(None, Some(&id), None, true)?,
                                CommentMutation::CreateTextHighlight(page, start, end, contents) => {
                                    let spec = session.plan.get(page as usize).ok_or("Page is out of range")?;
                                    let quads = with_planned_page(document, spec, |source| crate::text_geometry::inspect_with_source(source, id, page, revision)?.highlight_bounds(start, end))?;
                                    session.proposed_text_highlight(page as usize, quads, contents.as_deref())?
                                }
                            };
                            if next == session.plan { return current_info(session, original, document); }
                            let rendered = load_note_document(pdfium.as_ref().map_err(Clone::clone)?, document, session, &next)?;
                            session.commit_comments(next);
                            let info=match current_info(session,original,document){Ok(info)=>info,Err(error)=>{session.restore(snapshot);return Err(error);}};
                            let current_page=durable_page.map_or(0,|page|page.min(session.plan.len().saturating_sub(1) as u32));match persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, current_page) { Ok(Some(record)) if record.active => { recovery_heads.insert(id,(record.source,record.generation)); }, Ok(Some(_)) => { recovery_heads.remove(&id); }, Ok(None) => {}, Err(error) => { if error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED) { if let Some(lock)=record_recovery_lock(recovery_root.as_deref(), &mut recovery_locked, id, revision, session){return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}:{id}:{}:{}",lock.requested_revision,lock.committed_revision));} } else { session.restore(snapshot); } return Err(error); } }
                            note_documents.insert(id, (session.revision, rendered)); cache.close(id);
                            Ok(info)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Split(id, revision, pages_per_file, folder, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Start splitting again.".into()); }
                            let parent = folder.parent().ok_or("Choose a new folder for the split PDFs.")?;
                            let name = folder.file_name().ok_or("Choose a new folder name for the split PDFs.")?;
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            crate::split::prepare_and_publish(session, parent, name, pages_per_file, |bytes, expected| {
                                if reply.is_closed() { return Err("Split was canceled.".into()); }
                                let document = engine.load_pdf_from_byte_slice(bytes, None).map_err(|error| format!("Output could not be opened: {error}"))?;
                                if document.pages().len() as usize != expected { return Err("Output page count differs from the split plan.".into()); }
                                for index in 0..document.pages().len() {
                                    let page = document.pages().get(index).map_err(|error| format!("Output page {} could not be read: {error}", index + 1))?;
                                    let (width, height) = (page.width().value, page.height().value);
                                    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 { return Err(format!("Output page {} has invalid dimensions.", index + 1)); }
                                    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| format!("Output page {} could not be rendered: {error}", index + 1))?;
                                    if bitmap.width() <= 0 || bitmap.height() <= 0 { return Err(format!("Output page {} has an invalid bitmap.", index + 1)); }
                                }
                                Ok(())
                            })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::FormFields(id, revision, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Refresh the form field list.".into()); }
                            let mut query=crate::forms::FormDocument::query(session,id);
                            if query.status=="supported" && query.fields.iter().any(|field|field.checked.is_some()||field.radio.is_some()||field.choice.is_some()) {
                                let values=documents.get(&id).and_then(|document|checked_engine_form_values(document,&query.fields));
                                if values.as_ref().is_none_or(|values|!filled_values_agree(&query.fields,values)) {query.status="unsupported";query.reason=Some("The form states disagree with the rendering engine.".into());query.fields.clear();}
                            }
                            Ok(query)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::CheckFormCopy(id, revision, values, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Refresh the form field list.".into()); }
                            crate::forms::FormDocument::parse(session)?.prepare(&values).map(|_| ())
                        })();
                        let _ = reply.send(result);
                    }
                    Request::FillFormCopy(id, revision, values, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, original) = sessions.get(&id).ok_or("Document is closed")?;
                            if session.revision != revision { return Err("Document changed. Refresh the form field list.".into()); }
                            if path == PathBuf::from(&original.path) { return Err("Choose a new filename to preserve the source PDF.".into()); }
                            let form = crate::forms::FormDocument::parse(session)?;
                            let expected = form.pages;
                            let (bytes, fields) = form.prepare(&values)?;
                            let checked = crate::forms::FormDocument::query(&EditSession::new(bytes.clone(), expected), 0);
                            if checked.status != "supported" || checked.fields.len() != fields.len() || checked.fields.iter().zip(&fields).any(|(actual, expected)| actual.name != expected.name || actual.value != expected.value || actual.checked != expected.checked || actual.page != expected.page) { return Err("Filled form field and appearance validation failed.".into()); }
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let document = engine.load_pdf_from_byte_vec(bytes.clone(), None).map_err(|error| format!("Filled PDF could not be opened: {error}"))?;
                            let pages = page_sizes(&document)?;
                            if pages.len() != expected { return Err("Filled PDF page count validation failed.".into()); }
                            let engine_fields = checked_engine_form_values(&document,&fields).ok_or("Filled PDF form could not be read consistently by the rendering engine.")?;
                            if !filled_values_agree(&fields, &engine_fields) { return Err("Filled PDF values disagree with the rendering engine.".into()); }
                            for index in 0..document.pages().len() {
                                let page = document.pages().get(index).map_err(|error| format!("Filled PDF page could not be read: {error}"))?;
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| format!("Filled PDF page could not be rendered: {error}"))?;
                                if bitmap.width() <= 0 || bitmap.height() <= 0 { return Err("Filled PDF has an invalid bitmap.".into()); }
                                if reply.is_closed() { return Err("Fill form was canceled.".into()); }
                            }
                            if reply.is_closed() { return Err("Fill form was canceled.".into()); }
                            write_new_file(&path, &bytes)?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(id, (EditSession::new(bytes, info.pages.len()), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let cleanup = UnclaimedReply::Document(saved.document.id); ReplyLease::new(saved, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    Request::CheckCombine(first, second, reply) => {
                        if reply.is_closed() { continue; }
                        let result = combine_sessions(&sessions, first, second).and_then(|(first, second)| crate::combine::validate_sources(first, second).map(|_| ()));
                        let _ = reply.send(result);
                    }
                    Request::CheckInsertion(target, donor, at, reply) => {
                        if reply.is_closed() { continue; }
                        let result = insertion_sessions(&sessions, target, donor).and_then(|(target, donor)| crate::combine::validate_insertion(target, donor, at).map(|_| ()));
                        let _ = reply.send(result);
                    }
                    Request::CheckReplacement(target, donor, start, count, reply) => {
                        if reply.is_closed() { continue; }
                        let result = replacement_sessions(&sessions, target, donor).and_then(|(target, donor)| crate::combine::validate_replacement(target, donor, start, count).map(|_| ()));
                        let _ = reply.send(result);
                    }
                    Request::Combine(..) | Request::InsertPages(..) | Request::ReplacePages(..) => unreachable!(),
                    Request::CreateCopy(first, second, copy, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (first, second) = match copy {
                                CopyOperation::Combine => combine_sessions(&sessions, first, second)?,
                                CopyOperation::Insert { .. } => insertion_sessions(&sessions, first, second)?,
                                CopyOperation::Replace { .. } => replacement_sessions(&sessions, first, second)?,
                            };
                            let operation = copy.name();
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let mut prepared = None;
                            let validate = |bytes: &[u8], expected| {
                                if reply.is_closed() { return Err(format!("{operation} was canceled.")); }
                                let document = engine.load_pdf_from_byte_vec(bytes.to_vec(), None).map_err(|error| format!("Output could not be opened: {error}"))?;
                                let pages = page_sizes(&document)?;
                                if pages.len() != expected { return Err(format!("Output page count differs from the {operation} plan.")); }
                                for index in 0..document.pages().len() {
                                    let page = document.pages().get(index).map_err(|error| format!("Output page {} could not be read: {error}", index + 1))?;
                                    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| format!("Output page {} could not be rendered: {error}", index + 1))?;
                                    if bitmap.width() <= 0 || bitmap.height() <= 0 { return Err(format!("Output page {} has an invalid bitmap.", index + 1)); }
                                }
                                if reply.is_closed() { return Err(format!("{operation} was canceled.")); }
                                prepared = Some((document, pages));
                                Ok(())
                            };
                            let bytes = match copy {
                                CopyOperation::Insert { at } => crate::combine::prepare_insertion_and_write(first, second, at, &path, validate)?,
                                CopyOperation::Replace { start, count } => crate::combine::prepare_replacement_and_write(first, second, start, count, &path, validate)?,
                                CopyOperation::Combine => crate::combine::prepare_and_write(first, second, &path, validate)?,
                            };
                            let (document, pages) = prepared.ok_or("New copy output was not validated")?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(id, (EditSession::new(bytes, info.pages.len()), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let cleanup = UnclaimedReply::Document(saved.document.id); ReplyLease::new(saved, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    Request::CreateImagePdf(source, path, options, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let mut prepared_document = None;
                            let prepared = crate::image_pdf::prepare_and_write(&source, &path, options, |prepared| {
                                if reply.is_closed() { return Err("Create PDF was canceled.".into()); }
                                let document = engine.load_pdf_from_byte_vec(prepared.bytes.clone(), None).map_err(|error| format!("Output could not be opened: {error}"))?;
                                let pages = page_sizes(&document)?;
                                if pages.len() != 1 || (pages[0].width - prepared.page_width).abs() > 0.02 || (pages[0].height - prepared.page_height).abs() > 0.02 { return Err("Output page dimensions differ from the image conversion plan.".into()); }
                                let page = document.pages().get(0).map_err(|error| format!("Output page could not be read: {error}"))?;
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| format!("Output page could not be rendered: {error}"))?;
                                if bitmap.width() <= 0 || bitmap.height() <= 0 { return Err("Output page has an invalid bitmap.".into()); }
                                if reply.is_closed() { return Err("Create PDF was canceled.".into()); }
                                prepared_document = Some((document, pages));
                                Ok(())
                            })?;
                            let (document, pages) = prepared_document.ok_or("Created PDF output was not validated")?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(id, (EditSession::new(prepared.bytes, 1), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let cleanup = UnclaimedReply::Document(saved.document.id); ReplyLease::new(saved, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    Request::CreateMultiImagePdf(prepared, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            if path.exists() || !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf")) { return Err("Choose a new .pdf filename; Create PDF never overwrites an existing file.".into()); }
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let document = engine.load_pdf_from_byte_vec(prepared.bytes.clone(), None).map_err(|error| format!("Output could not be opened: {error}"))?;
                            let pages = page_sizes(&document)?;
                            if pages.len() != prepared.pages.len() { return Err("Output page count differs from the image conversion plan.".into()); }
                            for (index, ((width, height), page_size)) in prepared.pages.iter().zip(&pages).enumerate() {
                                if (page_size.width - width).abs() > 0.02 || (page_size.height - height).abs() > 0.02 { return Err(format!("Output page {} dimensions differ from the image conversion plan.", index + 1)); }
                                let page = document.pages().get(index as i32).map_err(|error| format!("Output page {} could not be read: {error}", index + 1))?;
                                let bitmap = page.render_with_config(&PdfRenderConfig::new().set_target_width(64).set_maximum_height(64)).map_err(|error| format!("Output page {} could not be rendered: {error}", index + 1))?;
                                if bitmap.width() <= 0 || bitmap.height() <= 0 { return Err(format!("Output page {} has an invalid bitmap.", index + 1)); }
                            }
                            if reply.is_closed() { return Err("Create PDF was canceled.".into()); }
                            write_new_file(&path, &prepared.bytes)?;
                            let id = next_id; next_id += 1;
                            let info = DocumentInfo { id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(id, (EditSession::new(prepared.bytes, info.pages.len()), info.clone()));
                            documents.insert(id, std::rc::Rc::new(document));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let cleanup = UnclaimedReply::Document(saved.document.id); ReplyLease::new(saved, cleanup, resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    Request::InspectImageReplacement(id, revision, page, reply) => {
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let original = documents.get(&id).ok_or("Document is closed")?;
                            checked_image_replacement(session, original, revision, page)
                        })();
                        let _ = reply.send(result);
                    }
                    Request::ReplaceImageCopy(id, revision, page, expected, replacement_rgb, path, reply) => {
                        if reply.is_closed() { continue; }
                        let result = (|| {
                            let (session, _) = sessions.get(&id).ok_or("Document is closed")?;
                            let original = documents.get(&id).ok_or("Document is closed")?;
                            let current = checked_image_replacement(session, original, revision, page)?;
                            if current != expected { return Err("The image replacement target changed. Inspect it again.".into()); }
                            let output_bytes = crate::image_edit::replace_flat_raster_image(&session.source, u32::from(page) + 1, &replacement_rgb)?;
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let output = engine.load_pdf_from_byte_vec(output_bytes.clone(), None).map_err(|error| format!("Image replacement output could not be opened: {error}"))?;
                            if output.pages().len() != original.pages().len() { return Err("Image replacement output page count changed.".into()); }
                            for index in 0..output.pages().len() {
                                let target = crate::image_edit::inspect_flat_raster_image(&session.source, u32::from(index as u16) + 1)?;
                                let render = |document: &PdfDocument<'_>| -> Result<Vec<u8>, String> {
                                    let page = document.pages().get(index).map_err(|error| error.to_string())?;
                                    let bitmap = page.render_with_config(&PdfRenderConfig::new().set_fixed_size(target.pixel_width as i32, target.pixel_height as i32).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE)).map_err(|error| error.to_string())?;
                                    let bytes = bitmap.as_raw_bytes();
                                    if bytes.chunks_exact(4).any(|pixel| pixel[3] != 255) { return Err("Image replacement render has unexpected alpha.".into()); }
                                    Ok(bytes.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0]]).collect())
                                };
                                let actual = render(&output)?;
                                if index == page as i32 { if actual != replacement_rgb { return Err("Image replacement pixels differ after reopening.".into()); } }
                                else if actual != render(original)? { return Err(format!("Image replacement changed unselected page {}.", index + 1)); }
                            }
                            if reply.is_closed() { return Err("Image replacement was canceled.".into()); }
                            write_new_file(&path, &output_bytes)?;
                            let pages = page_sizes(&output)?;
                            let new_id = next_id; next_id += 1;
                            let info = DocumentInfo { id: new_id, name: path.file_name().unwrap_or_default().to_string_lossy().into_owned(), path: path.to_string_lossy().into_owned(), pages, revision: 0, dirty: false, can_undo: false, can_redo: false };
                            sessions.insert(new_id, (EditSession::new(output_bytes, info.pages.len()), info.clone()));
                            documents.insert(new_id, std::rc::Rc::new(output));
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: info })
                        })();
                        let result = result.map(|saved| { let saved_id = saved.document.id; ReplyLease::new(saved, UnclaimedReply::Document(saved_id), resource_cleanup.clone()) });
                        if let Err(Ok(saved)) = reply.send(result) { documents.remove(&saved.document.id); sessions.remove(&saved.document.id); }
                    }
                    Request::InspectTextReplacement(id, revision, page, reply) => {
                        let result=(||{let(session,_)=sessions.get(&id).ok_or("Document is closed")?;let original=documents.get(&id).ok_or("Document is closed")?;checked_text_replacement(session,original,id,revision,page)})();let _=reply.send(result);
                    }
                    Request::ReplaceTextCopy(id, revision, page, expected, replacement, path, reply) => {
                        if reply.is_closed(){continue} let result=(||{
                            let(session,_)=sessions.get(&id).ok_or("Document is closed")?;let original=documents.get(&id).ok_or("Document is closed")?;let current=checked_text_replacement(session,original,id,revision,page)?;if current!=expected{return Err("The text replacement target changed. Inspect it again.".into())}
                            let bytes=crate::text_edit::replace_text_run(&session.source,u32::from(page)+1,&replacement)?;let engine=pdfium.as_ref().map_err(Clone::clone)?;let output=engine.load_pdf_from_byte_vec(bytes.clone(),None).map_err(|e|format!("Text replacement output could not be opened: {e}"))?;
                            let after_target=crate::text_edit::inspect_text_run(&bytes,u32::from(page)+1)?;let after=text_replacement_geometry(&output,id,revision,page,after_target)?;if after.target.text!=replacement{return Err("Text replacement extraction does not match the requested text.".into())}
                            verify_text_pixels(original,&output,page,&current.bounds,&after.bounds)?;
                            if reply.is_closed(){return Err("Text replacement was canceled.".into())} write_new_file(&path,&bytes)?;let pages=page_sizes(&output)?;let new_id=next_id;next_id+=1;let info=DocumentInfo{id:new_id,name:path.file_name().unwrap_or_default().to_string_lossy().into_owned(),path:path.to_string_lossy().into_owned(),pages,revision:0,dirty:false,can_undo:false,can_redo:false};sessions.insert(new_id,(EditSession::new(bytes,info.pages.len()),info.clone()));documents.insert(new_id,std::rc::Rc::new(output));Ok(SavedCopy{path:path.to_string_lossy().into_owned(),document:info})
                        })();let result=result.map(|saved|{let saved_id=saved.document.id;ReplyLease::new(saved,UnclaimedReply::Document(saved_id),resource_cleanup.clone())});if let Err(Ok(saved))=reply.send(result){documents.remove(&saved.document.id);sessions.remove(&saved.document.id);}
                    }
                    Request::Save(id, pages, path, reply) => {
                        let result = (|| {
                            let (session, original) = sessions.get_mut(&id).ok_or("Document is closed")?;
                            if path == PathBuf::from(&original.path) { return Err("Choose a new filename to preserve the source PDF.".into()); }
                            let bytes = session.export(pages.as_deref())?;
                            let engine = pdfium.as_ref().map_err(Clone::clone)?;
                            let check = engine.load_pdf_from_byte_slice(&bytes, None).map_err(|e| format!("Output validation failed: {e}"))?;
                            let expected = pages.as_ref().map_or(session.plan.len(), |p| p.iter().collect::<std::collections::HashSet<_>>().len());
                            if check.pages().len() as usize != expected { return Err("Output page count validation failed.".into()); }
                            write_new_file(&path, &bytes)?;
                            Ok(SavedCopy { path: path.to_string_lossy().into_owned(), document: current_info(session, original, documents.get(&id).ok_or("Document is closed")?)? })
                        })();
                        let _ = reply.send(result);
                    }
                    Request::Close(id, reply) => {
                        let result = (|| {
                            if let Some((session, original)) = sessions.get(&id) {
                                if !session.dirty() && recovery_heads.contains_key(&id) { persist_recovery_state(recovery_root.as_deref(), &mut recovery_floors, session, original, 0)?; }
                            }
                            documents.remove(&id); sessions.remove(&id); recovery_offers.remove(&id); recovery_heads.remove(&id); recovery_locked.remove(&id);
                            note_documents.remove(&id); cache.close(id); page_labels.close(id); Ok(())
                        })();
                        let _ = reply.send(result);
                    }
                }
            }
        }).expect("Could not start PDF worker");
        Self { sender }
    }
    pub async fn open(&self, path: PathBuf) -> Result<DocumentInfo, String> {
        let path = crate::open_path::gate_open_path(path).await?;
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Open(path, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?.map(ReplyLease::accept)
    }
    pub async fn begin_open(&self, path: PathBuf) -> Result<OpenResult, String> { self.begin_open_from(path, crate::open_path::OpenOrigin::Webview).await }
    pub async fn begin_open_from(&self, path: PathBuf, origin: crate::open_path::OpenOrigin) -> Result<OpenResult, String> {
        let path = crate::open_path::gate_open_path_from(path, origin).await?;
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::BeginOpen(path, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?.map(ReplyLease::accept)
    }
    #[allow(dead_code)]
    pub async fn checkpoint_recovery(&self, root: PathBuf, id: u64, revision: u64, current_page: u32) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckpointRecovery(root, id, revision, current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    #[allow(dead_code)]
    pub async fn restore_recovery(&self, root: PathBuf, path: PathBuf) -> Result<Option<RecoveredDocument>, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::RestoreRecovery(root, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(|value| value.map(ReplyLease::accept))
    }
    pub async fn keep_recovered_edits(&self, id: u64, revision: u64) -> Result<RecoveredDocument, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::KeepRecoveredEdits(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn open_original(&self, id: u64, revision: u64) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::OpenOriginal(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn discard_recovery(&self, id: u64, revision: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DiscardRecovery(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn begin_print(&self, id: u64, revision: u64) -> Result<PrintSnapshotInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::BeginPrint(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn end_print(&self, token: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::EndPrint(token, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn preflight_page_image(&self, id: u64, revision: u64, page: u16, dpi: u16, format: PageImageFormat) -> Result<PageImagePreflight, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PreflightPageImage(id, revision, page, dpi, format, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn export_page_image(&self, id: u64, revision: u64, page: u16, dpi: u16, format: PageImageFormat, path: PathBuf) -> Result<PageImageReceipt, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::ExportPageImage(id, revision, page, dpi, format, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn preflight_raster_redaction(&self, id: u64, revision: u64, page: u16, rectangles: Vec<crate::raster_redaction::RasterRedactionRect>) -> Result<RasterRedactionPreflight, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PreflightRasterRedaction(id, revision, page, rectangles, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn raster_redact(&self, id: u64, revision: u64, page: u16, rectangles: Vec<crate::raster_redaction::RasterRedactionRect>, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::RasterRedact(id, revision, page, rectangles, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    #[cfg(windows)]
    pub(crate) async fn preflight_searchable_ocr(&self, id: u64, revision: u64) -> Result<SearchableOcrPreflight, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PreflightSearchableOcr(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    #[cfg(windows)]
    pub(crate) async fn publish_searchable_ocr(&self, id: u64, revision: u64, path: PathBuf, prepared: crate::searchable_ocr_job::PreparedSearchableOcr) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PublishSearchableOcr(id, revision, path, prepared, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    #[cfg(windows)]
    pub(crate) async fn ocr_raster(&self, request: crate::ocr::OcrPageRequest, cancellation: crate::ocr_process::OcrCancellation, admission: crate::ocr_process::OcrWorkerHold) -> Result<crate::ocr::OcrPageRaster, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::OcrRaster(request, cancellation, admission, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    #[cfg(windows)]
    pub(crate) async fn validate_ocr_result(&self, request: crate::ocr::OcrPageRequest, admission: crate::ocr_process::OcrWorkerHold) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::ValidateOcrResult(request, admission, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub fn print_render_blocking(&self, token: u64, page: usize, max_width: u32, max_height: u32) -> Result<PrintBitmap, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PrintRender(token, page, max_width, max_height, tx)).map_err(|error| error.to_string())?; rx.blocking_recv().map_err(|error| error.to_string())?
    }
    pub async fn unlock(&self, id: u64, password: String) -> Result<OpenResult, String> {
        if password.len() > MAX_PASSWORD_BYTES { return Err("Passwords cannot be longer than 1 KiB.".into()); }
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Unlock(id, password, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?.map(ReplyLease::accept)
    }
    pub async fn cancel_password(&self, id: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CancelPassword(id, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn render(&self, id: u64, page: u16, width: i32) -> Result<Vec<u8>, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Render(id, page, width, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn close(&self, id: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Close(id, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn text(&self, id: u64, page: u16, revision: u64) -> Result<String, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Text(id, page, revision, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn text_geometry(&self, id: u64, page: u16, revision: u64) -> Result<crate::text_geometry::PageTextGeometry, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::TextGeometry(id, page, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn bookmarks(&self, id: u64, revision: u64) -> Result<BookmarkList, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Bookmarks(id, revision, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn page_labels(&self, id: u64, revision: u64) -> Result<crate::page_labels::DocumentPageLabels, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::PageLabels(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn properties(&self, id: u64, revision: u64) -> Result<crate::document_properties::DocumentProperties, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Properties(id, revision, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn form_fields(&self, id: u64, revision: u64) -> Result<crate::forms::FormFields, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::FormFields(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn check_form_copy(&self, id: u64, revision: u64, values: Vec<crate::forms::FieldValue>) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckFormCopy(id, revision, values, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn fill_form_copy(&self, id: u64, revision: u64, values: Vec<crate::forms::FieldValue>, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::FillFormCopy(id, revision, values, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn edit(&self, id: u64, edit: PageEdit, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableEdit(id, edit, current_page, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn edit_at(&self, id: u64, revision: u64, edit: PageEdit, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableEditAt(id, revision, edit, current_page, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn crop(&self, id: u64, page: u16, revision: u64, rect: CropRect, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableCrop(id, page, revision, rect, current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn create_image_pdf(&self, source: PathBuf, path: PathBuf, options: crate::image_pdf::ImagePdfOptions) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CreateImagePdf(source, path, options, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn create_multi_image_pdf(&self, prepared: crate::image_pdf::PreparedMultiImagePdf, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CreateMultiImagePdf(prepared, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn inspect_image_replacement(&self, id: u64, revision: u64, page: u16) -> Result<crate::image_edit::ImageEditTargetInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::InspectImageReplacement(id, revision, page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn replace_image_copy(&self, id: u64, revision: u64, page: u16, expected: crate::image_edit::ImageEditTargetInfo, replacement_rgb: Vec<u8>, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::ReplaceImageCopy(id, revision, page, expected, replacement_rgb, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn inspect_text_replacement(&self,id:u64,revision:u64,page:u16)->Result<TextReplacementInspection,String>{let(tx,rx)=oneshot::channel();self.sender.send(Request::InspectTextReplacement(id,revision,page,tx)).map_err(|e|e.to_string())?;rx.await.map_err(|e|e.to_string())?}
    pub async fn replace_text_copy(&self,id:u64,revision:u64,page:u16,expected:TextReplacementInspection,replacement:String,path:PathBuf)->Result<SavedCopy,String>{let(tx,rx)=oneshot::channel();self.sender.send(Request::ReplaceTextCopy(id,revision,page,expected,replacement,path,tx)).map_err(|e|e.to_string())?;rx.await.map_err(|e|e.to_string())?.map(ReplyLease::accept)}
    pub async fn crop_pages(&self, id: u64, pages: Vec<u16>, revision: u64, insets: CropInsets, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableCropPages(id, pages, revision, insets, current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn reset_crops(&self, id: u64, pages: Vec<u16>, revision: u64, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableResetCrops(id, pages, revision, current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn comments(&self, id: u64, revision: u64) -> Result<crate::comments::CommentList, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Comments(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn annotations(&self, id: u64, revision: u64) -> Result<crate::comments::AnnotationList, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Annotations(id, revision, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn create_text_highlight(&self, id: u64, revision: u64, page: u16, start: usize, end: usize, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::CreateTextHighlight(page, start, end, contents), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn create_highlight(&self, id: u64, revision: u64, page: u16, rect: CropRect, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::CreateHighlight(page, rect, contents), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn update_highlight(&self, id: u64, revision: u64, annotation_id: String, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::UpdateHighlight(annotation_id, contents), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn delete_highlight(&self, id: u64, revision: u64, annotation_id: String, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::DeleteHighlight(annotation_id), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn create_comment(&self, id: u64, revision: u64, page: u16, rect: CropRect, contents: String, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::Create(page, rect, contents), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn update_comment(&self, id: u64, revision: u64, note_id: String, contents: String, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::Update(note_id, contents), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn delete_comment(&self, id: u64, revision: u64, note_id: String, current_page: u32) -> Result<DocumentInfo, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::DurableComment(id, revision, CommentMutation::Delete(note_id), current_page, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn save_at(&self, id: u64, revision: u64, pages: Option<Vec<usize>>, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::SaveAt(id, revision, pages, path, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn check_revision(&self, id: u64, revision: u64) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckRevision(id, revision, tx)).map_err(|e| e.to_string())?; rx.await.map_err(|e| e.to_string())?
    }
    pub async fn save_copy_via<F, Fut>(&self, id: u64, revision: u64, pages: Option<Vec<usize>>, pick: F) -> Result<Option<SavedCopy>, String> where F: FnOnce(&'static str) -> Fut, Fut: std::future::Future<Output = Result<Option<PathBuf>, String>> {
        self.check_revision(id, revision).await?;
        let suggested = if pages.is_some() { "extracted-pages.pdf" } else { "organized-copy.pdf" };
        match pick(suggested).await? { Some(path) => self.save_at(id, revision, pages, path).await.map(Some), None => Ok(None) }
    }
    pub async fn split(&self, id: u64, revision: u64, pages_per_file: usize, folder: PathBuf) -> Result<crate::split::SplitOutput, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Split(id, revision, pages_per_file, folder, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn check_combine(&self, first: CombineSource, second: CombineSource) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckCombine(first, second, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn combine(&self, first: CombineSource, second: CombineSource, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::Combine(first, second, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn check_insertion(&self, target: CombineSource, donor: CombineSource, at: usize) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckInsertion(target, donor, at, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn insert_pages_copy(&self, target: CombineSource, donor: CombineSource, at: usize, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::InsertPages(target, donor, at, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
    pub async fn check_replacement(&self, target: CombineSource, donor: CombineSource, start: usize, count: usize) -> Result<(), String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::CheckReplacement(target, donor, start, count, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?
    }
    pub async fn replace_pages_copy(&self, target: CombineSource, donor: CombineSource, start: usize, count: usize, path: PathBuf) -> Result<SavedCopy, String> {
        let (tx, rx) = oneshot::channel(); self.sender.send(Request::ReplacePages(target, donor, start, count, path, tx)).map_err(|error| error.to_string())?; rx.await.map_err(|error| error.to_string())?.map(ReplyLease::accept)
    }
}

fn load_note_document<'a>(engine: &'a Pdfium, original: &std::rc::Rc<PdfDocument<'a>>, session: &EditSession, plan: &[PageSpec]) -> Result<std::rc::Rc<PdfDocument<'a>>, String> {
    match session.note_source(plan)? {
        None => Ok(original.clone()),
        Some(bytes) => {
            let document = engine.load_pdf_from_byte_vec(bytes, None).map_err(|error| error.to_string())?;
            if page_sizes(&document)?.len() != original.pages().len() as usize { return Err("Comment rendering changed the source page count. The edit was rejected.".into()); }
            Ok(std::rc::Rc::new(document))
        }
    }
}
fn note_document<'a>(engine: &'a Pdfium, original: &std::rc::Rc<PdfDocument<'a>>, session: &EditSession, documents: &mut HashMap<u64, (u64, std::rc::Rc<PdfDocument<'a>>)>, id: u64) -> Result<std::rc::Rc<PdfDocument<'a>>, String> {
    if let Some((revision, document)) = documents.get(&id) { if *revision == session.revision { return Ok(document.clone()); } }
    let document = load_note_document(engine, original, session, &session.plan)?;
    documents.insert(id, (session.revision, document.clone())); Ok(document)
}
fn displayed_note_rect(page: &PdfPage<'_>, rect: CropBox) -> Result<Option<crate::comments::DisplayRect>, String> {
    const SIZE: i32 = 1_000_000;
    let config = PdfRenderConfig::new().set_fixed_size(SIZE, SIZE); let mut points = Vec::with_capacity(4);
    for (x, y) in [(rect.left, rect.bottom), (rect.left, rect.top), (rect.right, rect.bottom), (rect.right, rect.top)] {
        points.push(page.points_to_pixels(PdfPoints::new(x), PdfPoints::new(y), &config).map_err(|error| error.to_string())?);
    }
    let x = (points.iter().map(|point| point.0).min().unwrap() as f64 / SIZE as f64).max(0.0);
    let y = (points.iter().map(|point| point.1).min().unwrap() as f64 / SIZE as f64).max(0.0);
    let right = (points.iter().map(|point| point.0).max().unwrap() as f64 / SIZE as f64).min(1.0);
    let bottom = (points.iter().map(|point| point.1).max().unwrap() as f64 / SIZE as f64).min(1.0);
    Ok((right > x && bottom > y).then_some(crate::comments::DisplayRect { x, y, width: right - x, height: bottom - y }))
}

fn combine_sessions(sessions: &HashMap<u64, (EditSession, DocumentInfo)>, first: CombineSource, second: CombineSource) -> Result<(&EditSession, &EditSession), String> {
    if first.id == second.id { return Err("Choose two different open PDFs to combine.".into()); }
    let first_session = &sessions.get(&first.id).ok_or("First document is closed")?.0;
    let second_session = &sessions.get(&second.id).ok_or("Second document is closed")?.0;
    if first_session.revision != first.revision || second_session.revision != second.revision { return Err("A document changed. Open Combine again.".into()); }
    Ok((first_session, second_session))
}

fn insertion_sessions(sessions: &HashMap<u64, (EditSession, DocumentInfo)>, target: CombineSource, donor: CombineSource) -> Result<(&EditSession, &EditSession), String> {
    if target.id == donor.id { return Err("Choose a different open PDF to insert into the target copy.".into()); }
    let target_session = &sessions.get(&target.id).ok_or("Target document is closed")?.0;
    let donor_session = &sessions.get(&donor.id).ok_or("Donor document is closed")?.0;
    if target_session.revision != target.revision || donor_session.revision != donor.revision { return Err("A document changed. Open Insert Pages again.".into()); }
    Ok((target_session, donor_session))
}

fn replacement_sessions(sessions: &HashMap<u64, (EditSession, DocumentInfo)>, target: CombineSource, donor: CombineSource) -> Result<(&EditSession, &EditSession), String> {
    if target.id == donor.id { return Err("Choose a different open PDF to replace pages in the target copy.".into()); }
    let target_session = &sessions.get(&target.id).ok_or("Target document is closed")?.0;
    let donor_session = &sessions.get(&donor.id).ok_or("Donor document is closed")?.0;
    if target_session.revision != target.revision || donor_session.revision != donor.revision { return Err("A document changed. Open Replace Pages again.".into()); }
    Ok((target_session, donor_session))
}

fn checked_read_text_bytes(current: usize, additional: usize, message: &str) -> Result<usize, String> {
    let total = current.checked_add(additional).ok_or_else(|| message.to_owned())?;
    if total > MAX_READ_TEXT_BYTES { return Err(message.into()); }
    Ok(total)
}

fn checked_page_image_request(session: &EditSession, document: &PdfDocument<'_>, revision: u64, page: u16, dpi: u16, format: PageImageFormat) -> Result<(crate::page_image::RasterDimensions, PageSpec), String> {
    if session.revision != revision { return Err("Document changed. Export the page image again.".into()); }
    if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) {
        return Err("Exporting images from encrypted or restricted PDFs is not supported in this build.".into());
    }
    session.page_image_export_guard()?;
    let spec = session.plan.get(usize::from(page)).ok_or_else(|| format!("{} export page is out of range", format.label()))?.clone();
    let dimensions = with_planned_page(document, &spec, |page| crate::page_image::dimensions(page.width().value, page.height().value, dpi, format))?;
    Ok((dimensions, spec))
}

struct RasterRedactionPagePlan {
    spec: PageSpec,
    dimensions: crate::page_image::RasterDimensions,
    page_width: f32,
    page_height: f32,
}

struct RasterRedactionPlan {
    pages: Vec<RasterRedactionPagePlan>,
}

fn checked_raster_redaction_request(
    session: &EditSession,
    document: &PdfDocument<'_>,
    revision: u64,
    selected_page: u16,
    rectangles: &[crate::raster_redaction::RasterRedactionRect],
) -> Result<RasterRedactionPlan, String> {
    if session.revision != revision { return Err("Document changed. Start redaction again.".into()); }
    if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) {
        return Err("Redaction of encrypted or restricted PDFs is not supported in this build.".into());
    }
    session.page_image_export_guard()?;
    if session.plan.is_empty() || session.plan.len() > 4_096 { return Err("Raster redaction supports 1 to 4,096 current pages.".into()); }
    if usize::from(selected_page) >= session.plan.len() { return Err("Redaction page is out of range.".into()); }
    let mut pages = Vec::with_capacity(session.plan.len());
    let mut total_pixels = 0u64;
    for spec in &session.plan {
        let (dimensions, page_width, page_height) = with_planned_page(document, spec, |page| {
            let page_width = page.width().value;
            let page_height = page.height().value;
            let dimensions = crate::page_image::dimensions(page_width, page_height, 150, PageImageFormat::Png)?;
            Ok((dimensions, page_width, page_height))
        })?;
        total_pixels = total_pixels.checked_add(u64::from(dimensions.width) * u64::from(dimensions.height)).ok_or("The redaction pixel count is out of range.")?;
        if total_pixels > 32_000_000 { return Err("The redacted copy exceeds the 32 megapixel aggregate limit at 150 DPI.".into()); }
        pages.push(RasterRedactionPagePlan { spec: spec.clone(), dimensions, page_width, page_height });
    }
    let selected = &pages[usize::from(selected_page)];
    crate::raster_redaction::validate_displayed_rectangles(selected.page_width, selected.page_height, selected.dimensions.width, selected.dimensions.height, rectangles)?;
    Ok(RasterRedactionPlan { pages })
}

#[cfg(windows)]
fn checked_ocr_request(
    session: &EditSession,
    document: &PdfDocument<'_>,
    request: crate::ocr::OcrPageRequest,
) -> Result<(crate::ocr::OcrRasterDimensions, PageSpec, f32, f32), String> {
    if session.revision != request.revision { return Err("Document changed. Start OCR again.".into()); }
    if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) {
        return Err("OCR of encrypted or restricted PDFs is not supported in this build.".into());
    }
    session.page_image_export_guard()?;
    let spec = session.plan.get(usize::from(request.page)).ok_or("OCR page is out of range")?.clone();
    let (dimensions, width, height) = with_planned_page(document, &spec, |page| {
        let width = page.width().value;
        let height = page.height().value;
        Ok((crate::ocr::raster_dimensions(width, height)?, width, height))
    })?;
    Ok((dimensions, spec, width, height))
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryOffer { revision: u64, current_page: u32 }
fn recovery_source_identity(bytes: &[u8]) -> Result<crate::recovery_journal::SourceIdentity, String> {
    let bytes_count = u64::try_from(bytes.len()).map_err(|_| "Recovery source is too large.")?;
    if bytes_count == 0 { return Err("Recovery source is empty.".into()); }
    Ok(crate::recovery_journal::SourceIdentity { bytes: bytes_count, sha256: Sha256::digest(bytes).into() })
}
fn recovery_rect(rect: CropBox) -> crate::recovery_journal::RecoveryRect {
    crate::recovery_journal::RecoveryRect { left: rect.left, bottom: rect.bottom, right: rect.right, top: rect.top }
}
fn recovery_record(session: &EditSession, source: crate::recovery_journal::SourceIdentity, generation: u64, current_page: u32, source_pages: usize) -> Result<crate::recovery_journal::RecoveryRecord, String> {
    let pages = session.plan.iter().map(|page| {
        let annotations = page.notes.iter().map(|note| {
            let kind = match (note.kind, note.quads.is_some()) {
                (crate::comments::AnnotationKind::Note, false) => crate::recovery_journal::RecoveryAnnotationKind::Note,
                (crate::comments::AnnotationKind::Highlight, false) => crate::recovery_journal::RecoveryAnnotationKind::AreaHighlight,
                (crate::comments::AnnotationKind::Highlight, true) => crate::recovery_journal::RecoveryAnnotationKind::TextHighlight,
                _ => return Err("Recovery annotation state is invalid.".into()),
            };
            Ok(crate::recovery_journal::RecoveryAnnotation { id: note.id.clone(), kind, rect: recovery_rect(note.rect), contents: note.contents.clone(), quads: note.quads.as_deref().unwrap_or(&[]).iter().copied().map(recovery_rect).collect() })
        }).collect::<Result<Vec<_>, String>>()?;
        Ok(crate::recovery_journal::RecoveryPage { source: u32::try_from(page.source).map_err(|_| "Recovery page mapping is too large.")?, turns: u8::try_from(page.turns).map_err(|_| "Recovery rotation is invalid.")?, crop: page.crop.map(recovery_rect), annotations })
    }).collect::<Result<Vec<_>, String>>()?;
    Ok(crate::recovery_journal::RecoveryRecord { active: true, generation, revision: session.revision, current_page, source_pages: u32::try_from(source_pages).map_err(|_| "Recovery source has too many pages.")?, source, edit_state: crate::recovery_journal::RecoveryEditState { pages } })
}
fn accept_recovery_record(floors: &mut HashMap<crate::recovery_journal::SourceIdentity, crate::recovery_journal::RecoveryRecord>, record: &crate::recovery_journal::RecoveryRecord) -> Result<(), String> {
    if let Some(previous) = floors.get(&record.source) {
        if record.generation < previous.generation || (record.generation == previous.generation && record != previous) {
            return Err("Recovery state moved backward or was replaced while the app was running.".into());
        }
    }
    floors.insert(record.source, record.clone());
    Ok(())
}
const RECOVERY_COMMITTED_REOPEN_REQUIRED: &str = "RECOVERY_COMMITTED_REOPEN_REQUIRED";
#[derive(Clone)]
struct RecoveryLock { requested_revision: u64, committed_revision: u64, source: crate::recovery_journal::SourceIdentity, generation: u64 }
fn record_recovery_lock(root: Option<&std::path::Path>, locks: &mut HashMap<u64, RecoveryLock>, id: u64, requested_revision: u64, session: &EditSession) -> Option<RecoveryLock> {
    let root = root?;
    let source = recovery_source_identity(&session.source).ok()?;
    let record = crate::recovery_store::read_recovery(root, source, None, 0).ok()??;
    let lock = RecoveryLock { requested_revision, committed_revision: record.revision, source, generation: record.generation };
    locks.insert(id, lock.clone());
    Some(lock)
}
fn write_recovery_verified(root: &std::path::Path, record: &crate::recovery_journal::RecoveryRecord) -> Result<(), String> {
    crate::recovery_store::write_recovery_detailed(root, record).map(|_| ()).map_err(|error| {
        if error.published { format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Recovery was published but could not be verified. Close and reopen this PDF.") } else { error.message }
    })
}
fn persist_recovery_state(root: Option<&std::path::Path>, floors: &mut HashMap<crate::recovery_journal::SourceIdentity, crate::recovery_journal::RecoveryRecord>, session: &EditSession, original: &DocumentInfo, current_page: u32) -> Result<Option<crate::recovery_journal::RecoveryRecord>, String> {
    let Some(root) = root else { return Ok(None); };
    let disk = std::fs::read(&original.path).map_err(|_| "The source PDF changed or became unavailable; recovery was not updated.".to_owned())?;
    if disk != session.source { return Err("The source PDF changed outside the app; recovery was not updated.".into()); }
    let source = recovery_source_identity(&session.source)?;
    let current = crate::recovery_store::read_recovery(root, source, None, 0)?;
    if let Some(record) = &current { accept_recovery_record(floors, record)?; }
    if !session.dirty() && current.is_none() { return Ok(None); }
    let generation = current.as_ref().map_or(Ok(1), |record| record.generation.checked_add(1).ok_or("Recovery generation is exhausted."))?;
    let record = if session.dirty() {
        recovery_record(session, source, generation, current_page, original.pages.len())?
    } else {
        crate::recovery_journal::RecoveryRecord { active: false, generation, revision: session.revision, current_page: 0, source_pages: u32::try_from(original.pages.len()).map_err(|_| "Recovery source has too many pages.")?, source, edit_state: crate::recovery_journal::RecoveryEditState { pages: Vec::new() } }
    };
    write_recovery_verified(root, &record)?;
    let verified = crate::recovery_store::read_recovery(root, source, None, record.revision).map_err(|_| format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Recovery was verified when published but its confirmation could not be repeated. Close and reopen this PDF."))?.ok_or_else(|| format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Recovery was verified when published but is no longer readable. Close and reopen this PDF."))?;
    if verified != record { return Err(format!("{RECOVERY_COMMITTED_REOPEN_REQUIRED}: Recovery changed after publication. Close and reopen this PDF.")); }
    accept_recovery_record(floors, &verified)?;
    Ok(Some(record))
}
fn recovery_checkpoint_current(root: &std::path::Path, head: Option<&(crate::recovery_journal::SourceIdentity, u64)>, session: &EditSession, original: &DocumentInfo, current_page: u32) -> Result<bool, String> {
    let source = recovery_source_identity(&session.source)?;
    let Some(current) = crate::recovery_store::read_recovery(root, source, None, 0)? else { return Ok(false); };
    if !current.active || head != Some(&(source, current.generation)) || current.revision != session.revision || current.current_page != current_page { return Ok(false); }
    if recovery_record(session, source, current.generation, current_page, original.pages.len())? != current { return Ok(false); }
    let disk = std::fs::read(&original.path).map_err(|_| "The source PDF changed or became unavailable; recovery was not updated.".to_owned())?;
    if disk != session.source { return Err("The source PDF changed outside the app; recovery was not updated.".into()); }
    Ok(true)
}
fn ensure_recovery_owner(root: Option<&std::path::Path>, heads: &HashMap<u64, (crate::recovery_journal::SourceIdentity, u64)>, floors: &mut HashMap<crate::recovery_journal::SourceIdentity, crate::recovery_journal::RecoveryRecord>, id: u64, session: &EditSession) -> Result<(), String> {
    let source = recovery_source_identity(&session.source)?;
    if heads.iter().any(|(owner, (identity, _))| *owner != id && *identity == source) { return Err("Another open tab owns recovery for this PDF. Close or resolve it before editing this tab.".into()); }
    if let Some(root) = root {
        if let Some(record) = crate::recovery_store::read_recovery(root, source, None, 0)? {
            accept_recovery_record(floors, &record)?;
            if record.active && heads.get(&id) != Some(&(source, record.generation)) { return Err("Recovery edits already exist for this PDF. Reopen it and choose which version to keep before editing.".into()); }
        }
    }
    Ok(())
}
fn restore_crop(rect: crate::recovery_journal::RecoveryRect) -> CropBox {
    CropBox { left: rect.left, bottom: rect.bottom, right: rect.right, top: rect.top }
}
fn restore_recovery_session(session: &mut EditSession, record: &crate::recovery_journal::RecoveryRecord) -> Result<(), String> {
    if usize::try_from(record.source_pages).ok() != Some(session.plan.len()) { return Err("Recovery source page count changed.".into()); }
    let plan = record.edit_state.pages.iter().map(|page| {
        let notes = page.annotations.iter().map(|annotation| {
            let (kind, quads) = match annotation.kind {
                crate::recovery_journal::RecoveryAnnotationKind::Note => (crate::comments::AnnotationKind::Note, None),
                crate::recovery_journal::RecoveryAnnotationKind::AreaHighlight => (crate::comments::AnnotationKind::Highlight, None),
                crate::recovery_journal::RecoveryAnnotationKind::TextHighlight => (crate::comments::AnnotationKind::Highlight, Some(annotation.quads.iter().copied().map(restore_crop).collect())),
            };
            crate::comments::Note { id: annotation.id.clone(), rect: restore_crop(annotation.rect), contents: annotation.contents.clone(), kind, quads }
        }).collect();
        Ok(PageSpec { source: usize::try_from(page.source).map_err(|_| "Recovery page mapping is invalid.")?, turns: i32::from(page.turns), crop: page.crop.map(restore_crop), notes })
    }).collect::<Result<Vec<_>, String>>()?;
    crate::comments::validate_plan(&plan)?;
    if plan == session.plan { return Err("Recovery record contains no unsaved edits.".into()); }
    session.revision = record.revision.checked_sub(1).ok_or("Recovery revision is invalid.")?;
    session.commit_comments(plan);
    if session.revision != record.revision { return Err("Recovery revision could not be restored.".into()); }
    Ok(())
}
type RenderKey=(u64,u16,i32,u64);
#[derive(Default)]struct RenderAdmission{epoch:u64,next:u64,keys:HashMap<RenderKey,u64>,replies:HashMap<u64,Reply<Vec<u8>>>}
const WORKER_QUEUE_LIMIT:usize=256;
#[derive(Debug)]enum SendRejected{Closed,Busy}
impl std::fmt::Display for SendRejected{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str(match self{SendRejected::Closed=>"sending on a closed channel",SendRejected::Busy=>"The PDF worker is busy with earlier requests. Try again."})}}
impl From<mpsc::SendError<Request>> for SendRejected{fn from(_:mpsc::SendError<Request>)->Self{SendRejected::Closed}}
#[derive(Clone)]struct RequestSender{inner:mpsc::Sender<Request>,renders:Arc<Mutex<RenderAdmission>>,limit:usize,pending:Arc<std::sync::atomic::AtomicUsize>,queue_limit:usize}
impl RequestSender{
    fn send_cleanup(&self,request:Request)->Result<(),SendRejected>{
        self.pending.fetch_add(1,std::sync::atomic::Ordering::AcqRel);
        self.inner.send(request).map_err(|error|{self.release();error.into()})
    }
    fn release(&self){let _=self.pending.fetch_update(std::sync::atomic::Ordering::AcqRel,std::sync::atomic::Ordering::Acquire,|n|n.checked_sub(1));}
    fn admit(&self,request:&Request)->bool{
        if matches!(request,Request::Close(..)|Request::CancelPassword(..)|Request::EndPrint(..)){self.pending.fetch_add(1,std::sync::atomic::Ordering::AcqRel);return true}
        self.pending.fetch_update(std::sync::atomic::Ordering::AcqRel,std::sync::atomic::Ordering::Acquire,|n|(n<self.queue_limit).then_some(n+1)).is_ok()
    }
    fn send(&self,request:Request)->Result<(),SendRejected>{
        if let Request::Render(id,page,width,reply)=request{
            let mut a=self.renders.lock().expect("render admission mutex poisoned");let key=(id,page,width,a.epoch);
            if let Some(token)=a.keys.get(&key).copied(){if let Some(old)=a.replies.insert(token,reply){let _=old.send(Err("Viewer render was superseded by a newer request.".into()));}return Ok(())}
            if a.keys.len()>=self.limit{let _=reply.send(Err("Viewer render queue is full. Try again.".into()));return Ok(())}
            a.next=a.next.wrapping_add(1);let token=a.next;a.keys.insert(key,token);a.replies.insert(token,reply);
            if self.inner.send(Request::AdmittedRender(id,page,width,token)).is_err(){a.keys.remove(&key);a.replies.remove(&token);return Err(SendRejected::Closed)}Ok(())
        }else{
            if !self.admit(&request){return Err(SendRejected::Busy)}
            {let mut a=self.renders.lock().expect("render admission mutex poisoned");a.epoch=a.epoch.wrapping_add(1);}
            self.inner.send(request).map_err(|error|{self.release();error.into()})
        }
    }
}

#[cfg(windows)]
fn checked_searchable_ocr_request(session: &EditSession, document: &PdfDocument<'_>, revision: u64) -> Result<usize, String> {
    if session.revision != revision { return Err("Document changed. Start searchable OCR again.".into()); }
    if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) { return Err("Searchable OCR of encrypted or restricted PDFs is not supported in this build.".into()); }
    session.page_image_export_guard()?;
    session.text_extraction_permission()?;
    if session.plan.is_empty() || session.plan.len() > crate::searchable_ocr_job::MAX_DOCUMENT_PAGES { return Err("Searchable OCR supports 1 to 32 current pages.".into()); }
    let mut pixels = 0_u64;
    for spec in &session.plan {
        let dimensions = with_planned_page(document, spec, |page| crate::ocr::raster_dimensions(page.width().value, page.height().value))?;
        pixels = pixels.checked_add(u64::from(dimensions.width).checked_mul(u64::from(dimensions.height)).ok_or("Searchable OCR pixel count overflows.")?).ok_or("Searchable OCR pixel count overflows.")?;
        if pixels > crate::searchable_pdf::MAX_SEARCHABLE_PIXELS { return Err("The searchable OCR copy exceeds the 32 MiPixel aggregate limit at 150 DPI.".into()); }
    }
    Ok(session.plan.len())
}

#[cfg(windows)]
fn checked_image_replacement(session: &EditSession, document: &PdfDocument<'_>, revision: u64, page: u16) -> Result<crate::image_edit::ImageEditTargetInfo, String> {
    if session.revision != revision { return Err("Document changed. Inspect the image again.".into()); }
    if !matches!(document.permissions().security_handler_revision(), Ok(PdfSecurityHandlerRevision::Unprotected)) { return Err("Image replacement does not support encrypted or restricted PDFs.".into()); }
    session.page_image_export_guard()?;
    if session.plan.len() != document.pages().len() as usize || session.plan.iter().enumerate().any(|(index, spec)| spec.source != index || spec.turns != 0 || spec.crop.is_some() || !spec.notes.is_empty()) {
        return Err("Image replacement requires the original unedited page order, geometry, and annotations.".into());
    }
    if usize::from(page) >= session.plan.len() { return Err("Image replacement page is out of range.".into()); }
    crate::image_edit::inspect_flat_raster_image(&session.source, u32::from(page) + 1)
}

#[cfg(windows)]
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextReplacementInspection { pub(crate) target: crate::text_edit::TextEditTarget, pub(crate) bounds: crate::text_edit_commands::TextReplacementBounds }

#[cfg(windows)]
fn text_replacement_geometry(document:&PdfDocument<'_>,id:u64,revision:u64,page:u16,target:crate::text_edit::TextEditTarget)->Result<TextReplacementInspection,String>{
    let pdf_page=document.pages().get(page as i32).map_err(|e|e.to_string())?;let geometry=crate::text_geometry::inspect(&pdf_page,id,page,revision)?;
    if geometry.status!="ok"||geometry.truncated||geometry.characters.len()!=target.text.len(){return Err("Text replacement requires complete one-byte PDFium character geometry.".into())}
    let extracted=geometry.characters.iter().map(|c|c.text.as_str()).collect::<String>();if extracted!=target.text{return Err("Text replacement source text disagrees with PDFium extraction.".into())}
    let mut left=1.0f32;let mut top=1.0f32;let mut right=0.0f32;let mut bottom=0.0f32;
    for c in &geometry.characters{if c.text.len()!=1||c.angle!=0{return Err("Text replacement requires one positioned cardinal glyph per source byte.".into())}let b=c.bounds.as_ref().ok_or("Text replacement requires bounds for every glyph.")?;left=left.min(b.x);top=top.min(b.y);right=right.max(b.x+b.width);bottom=bottom.max(b.y+b.height)}
    if right<=left||bottom<=top{return Err("Text replacement glyph bounds are invalid.".into())}
    Ok(TextReplacementInspection{target,bounds:crate::text_edit_commands::TextReplacementBounds{x:left,y:top,width:right-left,height:bottom-top}})
}

#[cfg(windows)]
fn checked_text_replacement(session:&EditSession,document:&PdfDocument<'_>,id:u64,revision:u64,page:u16)->Result<TextReplacementInspection,String>{
    if session.revision!=revision{return Err("Document changed. Inspect the text again.".into())}if !matches!(document.permissions().security_handler_revision(),Ok(PdfSecurityHandlerRevision::Unprotected)){return Err("Text replacement does not support encrypted or restricted PDFs.".into())}session.page_image_export_guard()?;
    if session.plan.len()!=document.pages().len() as usize||session.plan.iter().enumerate().any(|(i,s)|s.source!=i||s.turns!=0||s.crop.is_some()||!s.notes.is_empty()){return Err("Text replacement requires the original unedited page order, geometry, and annotations.".into())}if usize::from(page)>=session.plan.len(){return Err("Text replacement page is out of range.".into())}let target=crate::text_edit::inspect_text_run(&session.source,u32::from(page)+1)?;text_replacement_geometry(document,id,revision,page,target)
}

#[cfg(windows)]
fn verify_text_pixels(before:&PdfDocument<'_>,after:&PdfDocument<'_>,page:u16,a:&crate::text_edit_commands::TextReplacementBounds,b:&crate::text_edit_commands::TextReplacementBounds)->Result<(),String>{
    const SIDE:i32=1000;let render=|d:&PdfDocument<'_>|->Result<Vec<u8>,String>{let p=d.pages().get(page as i32).map_err(|e|e.to_string())?;let bitmap=p.render_with_config(&PdfRenderConfig::new().set_fixed_size(SIDE,SIDE).set_format(PdfBitmapFormat::BGRA).set_reverse_byte_order(false).clear_before_rendering(true).set_clear_color(PdfColor::WHITE)).map_err(|e|e.to_string())?;Ok(bitmap.as_raw_bytes().to_vec())};let x0=((a.x.min(b.x)*SIDE as f32).floor() as i32-1).max(0);let y0=((a.y.min(b.y)*SIDE as f32).floor() as i32-1).max(0);let x1=(((a.x+a.width).max(b.x+b.width)*SIDE as f32).ceil() as i32+1).min(SIDE);let y1=(((a.y+a.height).max(b.y+b.height)*SIDE as f32).ceil() as i32+1).min(SIDE);let lhs=render(before)?;let rhs=render(after)?;if lhs.len()!=rhs.len(){return Err("Text replacement render dimensions changed.".into())}for y in 0..SIDE{for x in 0..SIDE{if (x<x0||x>=x1||y<y0||y>=y1)&&lhs[((y*SIDE+x)*4)as usize..][..4]!=rhs[((y*SIDE+x)*4)as usize..][..4]{return Err("Text replacement changed pixels outside the glyph ink bounds.".into())}}}Ok(())
}

#[cfg(windows)]
fn publish_searchable_file(
    path: &PathBuf,
    bytes: &[u8],
    deadline: std::time::Instant,
    cancellation: &crate::ocr_process::OcrCancellation,
    before_commit: Option<&dyn Fn()>,
    after_commit: Option<&dyn Fn()>,
) -> Result<(), String> {
    if path.exists() { return Err("That file already exists. Choose a new filename; Save a Copy never overwrites an existing file.".into()); }
    if !path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("pdf")) { return Err("The output filename must end in .pdf.".into()); }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).ok_or("Choose an output folder.")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary.write_all(bytes).map_err(|error| error.to_string())?;
    temporary.as_file().sync_all().map_err(|error| error.to_string())?;
    if let Some(hook) = before_commit { hook(); }
    if std::time::Instant::now() >= deadline { return Err("OCR exceeded its time limit.".into()); }
    cancellation.commit().map_err(|error| error.to_string())?;
    if let Some(hook) = after_commit { hook(); }
    temporary.persist_noclobber(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(windows)]
fn validate_ocr_result(
    sessions: &HashMap<u64, (EditSession, DocumentInfo)>,
    documents: &HashMap<u64, std::rc::Rc<PdfDocument<'_>>>,
    request: crate::ocr::OcrPageRequest,
) -> Result<(), String> {
    let (session, _) = sessions.get(&request.document_id).ok_or("Document is closed")?;
    if !documents.contains_key(&request.document_id) { return Err("Document is closed".into()); }
    if session.revision != request.revision { return Err("Document changed. Start OCR again.".into()); }
    session.plan.get(usize::from(request.page)).ok_or("OCR page is out of range")?;
    Ok(())
}

fn print_dimensions(width: f32, height: f32, max_width: u32, max_height: u32) -> Result<(u32, u32), String> {
    if max_width == 0 || max_height == 0 || !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 { return Err("Invalid print dimensions.".into()); }
    let limit_width = max_width.min(4096) as f64;
    let limit_height = max_height.min(4096) as f64;
    let scale = (limit_width / width as f64).min(limit_height / height as f64).min((16_000_000.0 / (width as f64 * height as f64)).sqrt());
    let output_width = (width as f64 * scale).floor().max(1.0) as u32;
    let output_height = (height as f64 * scale).floor().max(1.0) as u32;
    Ok((output_width, output_height))
}

fn filled_values_agree(fields: &[crate::forms::FormField], engine_fields: &HashMap<String, Option<String>>) -> bool {
    engine_fields.len() == fields.len() && fields.iter().all(|field| engine_fields.get(&field.name).is_some_and(|value| value.as_deref().unwrap_or("") == field.value))
}
fn checked_engine_form_values(document: &PdfDocument<'_>, fields: &[crate::forms::FormField]) -> Option<HashMap<String, Option<String>>> {
    document.form()?;
    let expected=fields.iter().map(|field|(field.name.as_str(),field)).collect::<HashMap<_,_>>();
    let mut values=HashMap::new();
    let mut radio_indices:HashMap<&str,HashSet<usize>>=HashMap::new();
    for page in document.pages().iter() {
        for annotation in page.annotations().iter() {
            if let Some(field)=annotation.as_form_field() {
                let name=field.name()?;let expected=*expected.get(name.as_str())?;
                if let Some(choice)=&expected.choice {
                    let options=if choice.presentation=="dropdown"{let actual=field.as_combo_box_field()?;if actual.has_editable_text_box()||actual.is_multiselect(){return None;}actual.options()}else{let actual=field.as_list_box_field()?;if actual.is_multiselect(){return None;}actual.options()};
                    if options.len()!=choice.options.len(){return None;}
                    for(index,option)in choice.options.iter().enumerate(){let actual=options.get(index).ok()?;if actual.index()!=index||actual.label().map(String::as_str)!=Some(option.label.as_str())||actual.is_set()!=(choice.selected_option_id.as_ref()==Some(&option.option_id)){return None;}}
                    if values.insert(name,Some(expected.value.clone())).is_some(){return None;}continue;
                }
                if let Some(radio)=&expected.radio {
                    let actual=field.as_radio_button_field()?;let index=actual.index_in_group() as usize;let option=radio.options.get(index)?;
                    if !radio_indices.entry(expected.name.as_str()).or_default().insert(index){return None;}
                    let group_value=actual.group_value();let matches=if radio.selected_option_id.is_none(){group_value.as_deref().is_none_or(|value|value.is_empty()||value=="Off")}else{group_value.as_deref()==Some(expected.value.as_str())};
                    // The convenience fallback reports Off == Off as checked for blank groups.
                    // Their canonical parser proof requires every AS Off; verify the engine's
                    // blank group value and full child bijection instead. Selected groups still
                    // require every engine checked state to agree with the selected child.
                    if !matches || radio.selected_option_id.is_some() && actual.is_checked().ok()? != (radio.selected_option_id.as_ref()==Some(&option.option_id)){return None;}
                    values.insert(name,Some(expected.value.clone()));continue;
                }
                let value=if expected.checked.is_some() {
                    // pdfium-render 0.9.4 is_checked() recognizes only Yes. Read the actual
                    // engine value and compare with the exact state validated in the source AP.
                    let actual=field.as_checkbox_field()?.group_value()?;
                    let checked=if actual=="Off" {false} else if actual==*expected.on_state.as_ref()? {true} else {return None;};
                    Some(checked.to_string())
                } else {field.as_text_field()?.value()};
                if values.insert(name,value).is_some() {return None;}
            }
        }
    }
    if fields.iter().filter_map(|field|field.radio.as_ref().map(|radio|(field.name.as_str(),radio.options.len()))).any(|(name,count)|radio_indices.get(name).is_none_or(|indices|indices.len()!=count)){return None;}
    Some(values)
}

fn page_sizes(document: &PdfDocument<'_>) -> Result<Vec<PageSize>, String> {
    let pages = document.pages();
    if pages.is_empty() { return Err("This PDF has no pages.".into()); }
    if pages.len() > u16::MAX as i32 + 1 { return Err("This PDF exceeds the supported limit of 65,536 pages.".into()); }
    (0..pages.len()).map(|index| {
        let page = pages.get(index).map_err(|error| format!("Unable to read page {}: {error}", index + 1))?;
        let size = PageSize { width: page.width().value, height: page.height().value };
        if !size.width.is_finite() || !size.height.is_finite() || size.width <= 0.0 || size.height <= 0.0 { return Err(format!("Invalid dimensions on page {}.", index + 1)); }
        Ok(size)
    }).collect()
}

fn with_planned_page<T>(document: &PdfDocument<'_>, spec: &PageSpec, action: impl FnOnce(&PdfPage<'_>) -> Result<T, String>) -> Result<T, String> {
    let mut page = document.pages().get(spec.source as i32).map_err(|error| error.to_string())?;
    let original_rotation = page.rotation().map_err(|error| error.to_string())?;
    let original_crop = if spec.crop.is_some() {
        Some(page.boundaries().crop().or_else(|_| page.boundaries().bounding()).map_err(|error| error.to_string())?.bounds)
    } else { None };
    if let Some(crop) = spec.crop { page.boundaries_mut().set_crop(PdfRect::new_from_values(crop.bottom, crop.left, crop.top, crop.right)).map_err(|error| error.to_string())?; }
    let turns = (original_rotation.as_degrees() as i32 / 90 + spec.turns).rem_euclid(4);
    page.set_rotation(match turns { 1 => PdfPageRenderRotation::Degrees90, 2 => PdfPageRenderRotation::Degrees180, 3 => PdfPageRenderRotation::Degrees270, _ => PdfPageRenderRotation::None });
    let result = action(&page);
    let restore = match original_crop { Some(crop) => page.boundaries_mut().set_crop(crop).map_err(|error| error.to_string()), None => Ok(()) };
    page.set_rotation(original_rotation);
    result.and_then(|value| restore.map(|_| value))
}

fn validate_crop_insets(insets: CropInsets) -> Result<(), String> {
    let values = [insets.top, insets.right, insets.bottom, insets.left];
    if values.iter().any(|value| !value.is_finite() || *value < 0.0) {
        return Err("Enter finite, non-negative inset values in points.".into());
    }
    if values.iter().all(|value| *value == 0.0) { return Err("Enter at least one positive inset to crop the selected pages.".into()); }
    Ok(())
}

fn displayed_inset_rect(spec: &PageSpec, document: &PdfDocument<'_>, insets: CropInsets) -> Result<CropRect, String> {
    with_planned_page(document, spec, |page| {
        let width = f64::from(page.width().value);
        let height = f64::from(page.height().value);
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 { return Err("The page has invalid displayed dimensions.".into()); }
        let retained_width = width - insets.left - insets.right;
        let retained_height = height - insets.top - insets.bottom;
        if retained_width < 1.0 || retained_height < 1.0 { return Err("Insets must leave at least 1 point in both dimensions on every selected page.".into()); }
        Ok(CropRect { x: insets.left / width, y: insets.top / height, width: retained_width / width, height: retained_height / height })
    })
}

fn checked_displayed_crop(document: &PdfDocument<'_>, spec: &PageSpec, rect: CropRect) -> Result<CropBox, String> {
    let crop = with_planned_page(document, spec, |page| displayed_crop(page, rect))?;
    let mut proposed = spec.clone(); proposed.crop = Some(crop);
    with_planned_page(document, &proposed, |page| {
        let (width, height) = (page.width().value, page.height().value);
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 { return Err("The crop produces invalid page dimensions.".into()); }
        Ok(())
    })?;
    Ok(crop)
}

fn displayed_crop(page: &PdfPage<'_>, rect: CropRect) -> Result<CropBox, String> {
    if ![rect.x, rect.y, rect.width, rect.height].iter().all(|value| value.is_finite()) || rect.x < 0.0 || rect.y < 0.0 || rect.width <= 0.0 || rect.height <= 0.0 || rect.x + rect.width > 1.0 + 1e-12 || rect.y + rect.height > 1.0 + 1e-12 {
        return Err("Choose a nonempty crop rectangle within the current page.".into());
    }
    let bounds = page.boundaries().bounding().map_err(|error| error.to_string())?.bounds;
    let visible = CropBox { left: bounds.left().value, bottom: bounds.bottom().value, right: bounds.right().value, top: bounds.top().value };
    if rect.x == 0.0 && rect.y == 0.0 && rect.width == 1.0 && rect.height == 1.0 { visible.validate_within(visible)?; return Ok(visible); }
    const SIZE: i32 = 1_000_000;
    let config = PdfRenderConfig::new().set_fixed_size(SIZE, SIZE);
    // DeviceToPage takes integer pixels. Interpolate its page corners in f64 so a one-point crop is not rounded to a smaller pixel cell.
    let mut page_corners = [(0.0_f64, 0.0_f64); 4];
    for (index, (x, y)) in [(0, 0), (SIZE, 0), (0, SIZE), (SIZE, SIZE)].into_iter().enumerate() {
        let (x, y) = page.pixels_to_points(x, y, &config).map_err(|error| error.to_string())?;
        if !x.value.is_finite() || !y.value.is_finite() { return Err("The crop could not be mapped to the source page.".into()); }
        page_corners[index] = (f64::from(x.value), f64::from(y.value));
    }
    let mut crop = CropBox { left: f32::INFINITY, bottom: f32::INFINITY, right: f32::NEG_INFINITY, top: f32::NEG_INFINITY };
    for (x, y) in [(rect.x, rect.y), (rect.x + rect.width, rect.y), (rect.x, rect.y + rect.height), (rect.x + rect.width, rect.y + rect.height)] {
        let (x, y) = (x.min(1.0), y.min(1.0));
        let weights = [(1.0 - x) * (1.0 - y), x * (1.0 - y), (1.0 - x) * y, x * y];
        let point = weights.into_iter().zip(page_corners).fold((0.0, 0.0), |(x, y), (weight, (corner_x, corner_y))| (x + weight * corner_x, y + weight * corner_y));
        crop.left = crop.left.min(point.0 as f32); crop.bottom = crop.bottom.min(point.1 as f32); crop.right = crop.right.max(point.0 as f32); crop.top = crop.top.max(point.1 as f32);
    }
    crop.left = crop.left.max(visible.left); crop.bottom = crop.bottom.max(visible.bottom); crop.right = crop.right.min(visible.right); crop.top = crop.top.min(visible.top);
    crop.validate_within(visible)?;
    Ok(crop)
}

fn current_info(session: &EditSession, original: &DocumentInfo, document: &PdfDocument<'_>) -> Result<DocumentInfo, String> {
    let mut info = original.clone();
    info.pages = session.plan.iter().map(|spec| {
        if spec.crop.is_some() { return with_planned_page(document, spec, |page| Ok(PageSize { width: page.width().value, height: page.height().value })); }
        let size = &original.pages[spec.source];
        Ok(if spec.turns % 2 == 0 { size.clone() } else { PageSize { width: size.height, height: size.width } })
    }).collect::<Result<_, String>>()?;
    info.revision = session.revision; info.dirty = session.dirty(); info.can_undo = session.can_undo(); info.can_redo = session.can_redo(); Ok(info)
}

#[cfg(test)]
#[path = "recovery_restart_tests.rs"]
mod recovery_restart_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageDecoder;
    // Print admission is a process-wide four-job resource; isolate only its tests, not the PDF worker or the full suite.
    static PRINT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static PASSWORD_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn print_test_lock() -> std::sync::MutexGuard<'static, ()> {
        PRINT_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    fn password_test_lock() -> std::sync::MutexGuard<'static, ()> {
        PASSWORD_TESTS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
    trait TestReplyValue { type Accepted; fn accept(self) -> Self::Accepted; }
    impl<T> TestReplyValue for ReplyLease<T> { type Accepted = T; fn accept(self) -> T { ReplyLease::accept(self) } }
    macro_rules! accepted_reply_values {
        ($($value:ty),* $(,)?) => { $(impl TestReplyValue for $value { type Accepted = Self; fn accept(self) -> Self { self } })* };
    }
    accepted_reply_values!((), Vec<u8>, Vec<u64>, Vec<Option<String>>, String, DocumentInfo, SavedCopy, OpenResult, RecoveredDocument, PrintSnapshotInfo, PrintBitmap, PageImagePreflight, RasterRedactionPreflight, SearchableOcrPreflight, PageImageReceipt, BookmarkList, RenderWork, BookmarkWork, OcrWork, OcrWorkerState, crate::ocr::OcrPageRaster, crate::image_edit::ImageEditTargetInfo, TextReplacementInspection, crate::page_labels::DocumentPageLabels, crate::document_properties::DocumentProperties, crate::text_geometry::PageTextGeometry, crate::split::SplitOutput, crate::comments::CommentList, crate::comments::AnnotationList, crate::forms::FormFields);
    fn call<T: TestReplyValue>(service: &PdfService, request: impl FnOnce(Reply<T>) -> Request) -> Result<T::Accepted, String> {
        let (tx, rx) = oneshot::channel(); service.sender.send(request(tx)).unwrap(); rx.blocking_recv().unwrap().map(TestReplyValue::accept)
    }
    fn ocr_raster(service: &PdfService, request: crate::ocr::OcrPageRequest, cancellation: crate::ocr_process::OcrCancellation) -> Result<crate::ocr::OcrPageRaster, String> {
        let operation = crate::ocr_process::OcrOperation::try_begin(&cancellation).map_err(|error| error.to_string())?;
        let result = call(service, |reply| Request::OcrRaster(request, cancellation, operation.worker_hold(), reply));
        operation.finish(result.map_err(crate::ocr_process::OcrProcessError::Document)).map_err(|error| error.to_string())
    }
    fn p6_pixels(p6: &[u8]) -> (u32, u32, &[u8]) {
        let mut lines = p6.splitn(4, |byte| *byte == b'\n');
        assert_eq!(lines.next(), Some(b"P6".as_slice()));
        let dimensions = std::str::from_utf8(lines.next().unwrap()).unwrap().split_once(' ').unwrap();
        assert_eq!(lines.next(), Some(b"255".as_slice()));
        (dimensions.0.parse().unwrap(), dimensions.1.parse().unwrap(), lines.next().unwrap())
    }
    #[test]
    fn save_copy_refuses_a_stale_revision_before_the_dialog_opens() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join("resources/welcome.pdf");
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        let stale = info.revision;
        let edited = call(&service, |reply| Request::DurableEditAt(info.id, stale, PageEdit::Rotate { pages: vec![0], clockwise: true }, 0, reply)).unwrap();
        let dialogs = std::sync::atomic::AtomicUsize::new(0);
        let output = folder.path().join("copy.pdf");
        let refused = tauri::async_runtime::block_on(service.save_copy_via(info.id, stale, None, |_| { dialogs.fetch_add(1, std::sync::atomic::Ordering::SeqCst); let output = output.clone(); async move { Ok(Some(output)) } }));
        assert_eq!(refused.err().unwrap(), STALE_PAGE_PLAN);
        assert_eq!(dialogs.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(!output.exists());
        let accepted = tauri::async_runtime::block_on(service.save_copy_via(info.id, edited.revision, Some(vec![0]), |name| { dialogs.fetch_add(1, std::sync::atomic::Ordering::SeqCst); assert_eq!(name, "extracted-pages.pdf"); let output = output.clone(); async move { Ok(Some(output)) } }));
        assert!(accepted.unwrap().is_some());
        assert_eq!(dialogs.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(output.exists());
        let cancelled = tauri::async_runtime::block_on(service.save_copy_via(info.id, edited.revision, None, |_| async { Ok(None) }));
        assert!(cancelled.unwrap().is_none());
        let closed = tauri::async_runtime::block_on(service.save_copy_via(info.id + 99, 0, None, |_| async { Ok(None) }));
        assert!(closed.is_err());
    }
    #[test]
    fn page_edit_and_save_reject_stale_revisions_and_accept_the_current_one() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join("resources/welcome.pdf");
        let source = std::fs::read(&path).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        let stale = info.revision;
        info = call(&service, |reply| Request::DurableEditAt(info.id, stale, PageEdit::Rotate { pages: vec![0], clockwise: true }, 0, reply)).unwrap();
        assert_ne!(info.revision, stale);
        let (count, current) = (info.pages.len(), info.revision);
        for kind in 0..3 {
            let edit = match kind { 0 => PageEdit::Rotate { pages: vec![1], clockwise: true }, 1 => PageEdit::Delete { pages: vec![1] }, _ => PageEdit::Move { from: 1, to: 2 } };
            let error = call(&service, |reply| Request::DurableEditAt(info.id, stale, edit, 0, reply)).err().unwrap();
            assert_eq!(error, STALE_PAGE_PLAN);
        }
        let stale_copy = folder.path().join("stale.pdf");
        assert_eq!(call(&service, |reply| Request::SaveAt(info.id, stale, Some(vec![0]), stale_copy.clone(), reply)).err().unwrap(), STALE_PAGE_PLAN);
        assert_eq!(call(&service, |reply| Request::SaveAt(info.id, stale, None, stale_copy.clone(), reply)).err().unwrap(), STALE_PAGE_PLAN);
        assert!(!stale_copy.exists());
        assert_eq!(call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap().pages.len(), count);
        let moved = call(&service, |reply| Request::DurableEditAt(info.id, current, PageEdit::Move { from: 0, to: 1 }, 0, reply)).unwrap();
        let deleted = call(&service, |reply| Request::DurableEditAt(info.id, moved.revision, PageEdit::Delete { pages: vec![1] }, 0, reply)).unwrap();
        assert_eq!(deleted.pages.len(), count - 1);
        assert_eq!(call(&service, |reply| Request::DurableEditAt(info.id, current, PageEdit::Undo, 0, reply)).err().unwrap(), STALE_PAGE_PLAN);
        let extracted = folder.path().join("extracted.pdf");
        call(&service, |reply| Request::SaveAt(info.id, deleted.revision, Some(vec![0]), extracted.clone(), reply)).unwrap();
        assert!(extracted.exists());
        let full = folder.path().join("full.pdf");
        call(&service, |reply| Request::SaveAt(info.id, deleted.revision, None, full.clone(), reply)).unwrap();
        assert!(full.exists());
        assert_eq!(std::fs::read(&path).unwrap(), source);
    }
    #[test]
    fn page_labels_follow_current_plan_revision_and_close_without_changing_source() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let path = root.join("tests/fixtures/reportlab-page-labels.pdf");
        let source = std::fs::read(&path).unwrap();
        let library = root.join("resources/pdfium/bin/pdfium.dll");
        let expected = ["1", "2", "iv", "v", "I", "II", "AA", "BB", "bb", "cc", "Appendix", "Appendix", "N-5", "N-6"];
        let service = PdfService::start(library);
        let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        let engine_labels = call(&service, |reply| Request::EnginePageLabels(info.id, reply)).unwrap();
        assert_eq!(engine_labels, expected.map(|label| Some(label.to_owned())), "PDFium must independently agree with all fixture labels");
        assert_eq!(info.pages.len(), 14, "PDFium must agree with the 14-page fixture");
        let original = call(&service, |reply| Request::PageLabels(info.id, info.revision, reply)).unwrap();
        assert_eq!(original.status, crate::page_labels::PageLabelStatus::Supported);
        assert_eq!(original.labels.len(), 14);
        assert_eq!(original.labels[8].label, "bb");
        assert!(original.labels.iter().enumerate().all(|(page, label)| label.page == page));

        assert!(call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 8, to: 0 }, reply)).err().unwrap().contains("page labels"));
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![8], clockwise: true }, reply)).unwrap();
        assert!(call(&service, |reply| Request::PageLabels(info.id, 0, reply)).unwrap_err().contains("changed"));
        let rotated = call(&service, |reply| Request::PageLabels(info.id, info.revision, reply)).unwrap();
        assert_eq!(rotated.labels[0].label, "1");
        assert_eq!(rotated.labels[8].label, "bb");
        assert!(rotated.labels.iter().enumerate().all(|(page, label)| label.page == page));

        call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        assert!(call(&service, |reply| Request::PageLabels(info.id, info.revision, reply)).unwrap_err().contains("closed"));
        assert_eq!(std::fs::read(path).unwrap(), source);
    }
    #[test]
    fn page_labels_reads_preserve_all_corpus_sources_sessions_and_renders() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture);
            let source = std::fs::read(&path).unwrap();
            let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            assert_eq!(info.pages.len(), count);
            let before_first = call(&service, |reply| Request::Render(info.id, 0, 64, reply)).unwrap();
            let before_last = call(&service, |reply| Request::Render(info.id, (count - 1) as u16, 64, reply)).unwrap();
            for _ in 0..2 {
                let labels = call(&service, |reply| Request::PageLabels(info.id, 0, reply)).unwrap();
                assert_eq!(labels.status, crate::page_labels::PageLabelStatus::None);
                assert!(labels.labels.is_empty() && labels.reason.is_none());
            }
            assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 64, reply)).unwrap(), before_first);
            assert_eq!(call(&service, |reply| Request::Render(info.id, (count - 1) as u16, 64, reply)).unwrap(), before_last);
            let unchanged = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap();
            assert_eq!(unchanged.revision, 0);
            assert!(!unchanged.dirty && !unchanged.can_undo && !unchanged.can_redo);
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
            assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn forms_empty_values_require_an_actual_named_engine_field() {
        let expected = vec![crate::forms::FormField {field_id:"opaque".into(),name:"Name".into(),page:0,value:String::new(),max_length:None,checked:None,on_state:None,radio:None,choice:None}];
        assert!(filled_values_agree(&expected,&HashMap::from([("Name".into(),None)])), "PDFium's present empty field is valid");
        assert!(filled_values_agree(&expected,&HashMap::from([("Name".into(),Some(String::new()))])));
        assert!(!filled_values_agree(&expected,&HashMap::from([("Unknown".into(),None)])), "An unrelated empty field cannot substitute for the missing canonical name");
        assert!(!filled_values_agree(&expected,&HashMap::new()));
        assert!(!filled_values_agree(&expected,&HashMap::from([("Name".into(),Some("wrong".into()))])));
    }
    #[test]
    fn choices_engine_rotations_inherited_crops_labels_list_pixels_reopen_print_and_source() {
        let _print_lock=print_test_lock();let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for(rotation,inherited)in[0,90,180,270].into_iter().flat_map(|rotation|[false,true].map(move|inherited|(rotation,inherited))){let mut doc=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf")).unwrap();let page=*doc.get_pages().values().next().unwrap();let owner=if inherited{doc.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap()}else{page};if inherited{doc.get_dictionary_mut(page).unwrap().remove(b"CropBox");doc.get_dictionary_mut(page).unwrap().remove(b"Rotate");}doc.get_dictionary_mut(owner).unwrap().set("Rotate",rotation);doc.get_dictionary_mut(owner).unwrap().set("CropBox",vec![30.into(),450.into(),350.into(),760.into()]);let path=folder.path().join(format!("choice-source-{rotation}-{inherited}.pdf"));doc.save(&path).unwrap();let bytes=std::fs::read(&path).unwrap();let source=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let before=comments_png(&service,source.id,0,640);let snapshot=print_snapshot(&service,source.id,0);let before_print=call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap();let query=call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap();assert_eq!(query.status,"supported","{}",query.reason.unwrap_or_default());assert_eq!(query.fields.len(),2);assert_eq!(query.fields[0].value,"south-002");assert_eq!(query.fields[1].value,"email-002");let values=query.fields.iter().map(|field|crate::forms::FieldValue::Choice {field_id:field.field_id.clone(),option_id:Some(field.choice.as_ref().unwrap().options[0].option_id.clone())}).collect();let output_path=folder.path().join(format!("choice-filled-{rotation}-{inherited}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(source.id,0,values,output_path.clone(),reply)).unwrap();assert!(!output.document.dirty);assert_eq!(output.document.revision,0);let after=comments_png(&service,output.document.id,0,640);assert_eq!(before.dimensions(),after.dimensions());let(mut dropdown_changed,mut list_changed,mut outside,mut new_blue,mut old_blue,mut source_blue)=(0,0,0,0,0,0);
            for(x,y,pixel)in after.enumerate_pixels(){let(nx,ny)=(x as f32/after.width() as f32,y as f32/after.height() as f32);let(sx,sy)=match rotation{0=>(30.0+320.0*nx,760.0-310.0*ny),90=>(30.0+320.0*ny,450.0+310.0*nx),180=>(350.0-320.0*nx,450.0+310.0*ny),_=>(350.0-320.0*ny,760.0-310.0*nx)};let dropdown=(59.0..=281.0).contains(&sx)&&(663.0..=693.0).contains(&sy);let list=(59.0..=281.0).contains(&sx)&&(503.0..=605.0).contains(&sy);if pixel!=before.get_pixel(x,y){if dropdown{dropdown_changed+=1;}else if list{list_changed+=1;}else{outside+=1;}}let blue=|p:&image::Rgb<u8>|(140..=180).contains(&p.0[0])&&(175..=205).contains(&p.0[1])&&(200..=235).contains(&p.0[2]);if(70.0..=270.0).contains(&sx){if(588.0..=599.0).contains(&sy)&&blue(pixel){new_blue+=1;}if(572.0..=583.0).contains(&sy){if blue(pixel){old_blue+=1;}if blue(before.get_pixel(x,y)){source_blue+=1;}}}}
            assert!(dropdown_changed>30&&list_changed>1000&&new_blue>500&&source_blue>500,"Choice independent pixels {rotation}/{inherited}: dropdown {dropdown_changed}, list {list_changed}, new blue {new_blue}, source blue {source_blue}");assert_eq!(old_blue,0);assert_eq!(outside,0);assert_eq!(comments_png(&service,source.id,0,640),before);assert_eq!(std::fs::read(&path).unwrap(),bytes);let readback=call(&service,|reply|Request::FormFields(output.document.id,0,reply)).unwrap();assert_eq!(readback.fields[0].value,"north-001");assert_eq!(readback.fields[1].value,"print-001");let saved=lopdf::Document::load(&output_path).unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();for reference in doc.get_dictionary(form).unwrap().get(b"Fields").unwrap().as_array().unwrap(){let id=reference.as_reference().unwrap();let field=saved.get_dictionary(id).unwrap();assert_eq!(field.get(b"I").unwrap().as_array().unwrap(),&vec![lopdf::Object::Integer(0)]);for key in[b"Opt".as_slice(),b"DV",b"DA",b"BS",b"MK",b"Rect"]{assert_eq!(field.get(key).unwrap(),doc.get_dictionary(id).unwrap().get(key).unwrap());}}for(id,value)in&doc.objects{if !doc.get_dictionary(form).unwrap().get(b"Fields").unwrap().as_array().unwrap().contains(&lopdf::Object::Reference(*id)){assert_eq!(saved.get_object(*id).unwrap(),value);}}assert_eq!(call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap().bgra,before_print.bgra);let new_snapshot=print_snapshot(&service,output.document.id,0);assert_ne!(call(&service,|reply|Request::PrintRender(new_snapshot.token,0,640,640,reply)).unwrap().bgra,before_print.bgra);let reopened=call(&service,|reply|Request::Open(output_path.clone(),reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,0,640),after);
            if rotation==0&&!inherited{let proof=root.join("target/forms-probe");std::fs::write(proof.join("choice-cropped-source.pdf"),&bytes).unwrap();std::fs::copy(&output_path,proof.join("choice-filled.pdf")).unwrap();before.save(proof.join("choice-source-pdfium.png")).unwrap();after.save(proof.join("choice-filled-pdfium.png")).unwrap();}for id in[source.id,output.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
        }
        let path=root.join("tests/fixtures/reportlab-choice-blank-fields.pdf");let source=call(&service,|reply|Request::Open(path,reply)).unwrap();let query=call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap();assert_eq!(query.status,"supported","{}",query.reason.unwrap_or_default());assert!(query.fields.iter().all(|field|field.choice.as_ref().unwrap().selected_option_id.is_none()));let values=query.fields.iter().map(|field|crate::forms::FieldValue::Choice {field_id:field.field_id.clone(),option_id:Some(field.choice.as_ref().unwrap().options[2].option_id.clone())}).collect();let output=call(&service,|reply|Request::FillFormCopy(source.id,0,values,folder.path().join("from-blank.pdf"),reply)).unwrap();assert!(call(&service,|reply|Request::FormFields(output.document.id,0,reply)).unwrap().fields.iter().all(|field|field.value=="local-003"));assert!(call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap().fields.iter().all(|field|field.value.is_empty()));for id in[source.id,output.document.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
    }
    #[test]
    fn radios_engine_rotations_inherited_crops_switch_blank_reopen_print_and_source() {
        let _print_lock=print_test_lock();let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for(rotation,inherited)in[0,90,180,270].into_iter().flat_map(|rotation|[false,true].map(|inherited|(rotation,inherited))){
            let mut doc=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-radio-fields.pdf")).unwrap();let page=*doc.get_pages().values().next().unwrap();let owner=if inherited{doc.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap()}else{page};if inherited{let leaf=doc.get_dictionary_mut(page).unwrap();leaf.remove(b"Rotate");leaf.remove(b"CropBox");}let bounds=doc.get_dictionary_mut(owner).unwrap();bounds.set("Rotate",rotation);bounds.set("CropBox",vec![50.into(),550.into(),340.into(),760.into()]);let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let parent=doc.get_dictionary(form).unwrap().get(b"Fields").unwrap().as_array().unwrap()[0].as_reference().unwrap();doc.get_dictionary_mut(parent).unwrap().set("DV",lopdf::Object::Name(b"Choice C".to_vec()));let kids=doc.get_dictionary(parent).unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();let blank=rotation==270;if blank{doc.get_dictionary_mut(parent).unwrap().remove(b"V");for kid in&kids{doc.get_dictionary_mut(kid.as_reference().unwrap()).unwrap().set("AS",lopdf::Object::Name(b"Off".to_vec()));}}
            let path=folder.path().join(format!("radio-source-{rotation}-{inherited}.pdf"));doc.save(&path).unwrap();let bytes=std::fs::read(&path).unwrap();let source=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let before=comments_png(&service,source.id,0,640);let snapshot=print_snapshot(&service,source.id,0);let before_print=call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap();let query=call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap();assert_eq!(query.status,"supported","{}",query.reason.unwrap_or_default());let radio=query.fields[0].radio.as_ref().unwrap();assert_eq!(radio.selected_option_id.is_none(),blank);let values=vec![crate::forms::FieldValue::Radio {field_id:query.fields[0].field_id.clone(),option_id:Some(radio.options[0].option_id.clone())}];let output_path=folder.path().join(format!("radio-filled-{rotation}-{inherited}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(source.id,0,values,output_path.clone(),reply)).unwrap();assert!(!output.document.dirty);assert_eq!(output.document.revision,0);let after=comments_png(&service,output.document.id,0,640);let mut selected_ink=0;let mut previous_ink=0;let mut changed=0;let mut outside=0;
            for(x,y,pixel)in after.enumerate_pixels(){let(nx,ny)=(x as f32/after.width()as f32,y as f32/after.height()as f32);let(sx,sy)=match rotation{0=>(50.0+290.0*nx,760.0-210.0*ny),90=>(50.0+290.0*ny,550.0+210.0*nx),180=>(340.0-290.0*nx,550.0+210.0*ny),_=>(340.0-290.0*ny,760.0-210.0*nx)};let new=(66.0..=74.0).contains(&sx)&&(656.0..=664.0).contains(&sy);let old=(66.0..=74.0).contains(&sx)&&(616.0..=624.0).contains(&sy);let allowed=(58.0..=82.0).contains(&sx)&&((648.0..=672.0).contains(&sy)||(608.0..=632.0).contains(&sy));if new&&pixel.0.iter().all(|v|*v<100){selected_ink+=1;}if old&&pixel.0.iter().all(|v|*v<100){previous_ink+=1;}if pixel!=before.get_pixel(x,y){changed+=1;if!allowed{outside+=1;}}}
            assert!(selected_ink>80&&changed>150,"Independent selected radio interior {rotation}/{inherited}: ink {selected_ink}, changed {changed}");assert_eq!(previous_ink,0,"Switching must clear the old selection");assert_eq!(outside,0,"Only affected radio appearances may change");let saved=lopdf::Document::load(&output_path).unwrap();for(id,value)in&doc.objects{if matches!(value,lopdf::Object::Stream(_)){assert_eq!(saved.get_object(*id).unwrap(),value,"Every original raw AP and page stream is retained");}}assert_eq!(saved.get_dictionary(parent).unwrap().get(b"DV").unwrap().as_name().unwrap(),b"Choice C");assert_eq!(std::fs::read(&path).unwrap(),bytes);assert_eq!(comments_png(&service,source.id,0,640),before);let old_print=call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap();assert_eq!(old_print.bgra,before_print.bgra);let new_snapshot=print_snapshot(&service,output.document.id,0);assert_ne!(call(&service,|reply|Request::PrintRender(new_snapshot.token,0,640,640,reply)).unwrap().bgra,before_print.bgra);let reopened=call(&service,|reply|Request::Open(output_path.clone(),reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,0,640),after);let readback=call(&service,|reply|Request::FormFields(reopened.id,0,reply)).unwrap();assert_eq!(readback.fields[0].value,"Choice A");
            if rotation==0&&!inherited{let proof=root.join("target/forms-probe");std::fs::create_dir_all(&proof).unwrap();std::fs::write(proof.join("radio-cropped-source.pdf"),&bytes).unwrap();std::fs::copy(output_path,proof.join("radio-filled.pdf")).unwrap();before.save(proof.join("radio-source-pdfium.png")).unwrap();after.save(proof.join("radio-filled-pdfium.png")).unwrap();println!("Radio independent cross-engine proof: {}",proof.display());}for id in[source.id,output.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
        }
    }
    #[test]
    fn checkboxes_mixed_engine_rotations_crops_toggle_clear_appearances_and_print() {
        let _print_lock=print_test_lock();let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for (rotation,inherited) in [0,90,180,270].into_iter().flat_map(|rotation|[false,true].map(|inherited|(rotation,inherited))) {
            let mut doc=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf")).unwrap();let page=*doc.get_pages().values().next().unwrap();let owner=if inherited {doc.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap()} else {page};if inherited {let leaf=doc.get_dictionary_mut(page).unwrap();leaf.remove(b"Rotate");leaf.remove(b"CropBox");}let bounds=doc.get_dictionary_mut(owner).unwrap();bounds.set("Rotate",rotation);bounds.set("CropBox",vec![50.into(),550.into(),340.into(),760.into()]);
            let path=folder.path().join(format!("checkbox-source-{rotation}-{inherited}.pdf"));doc.save(&path).unwrap();let bytes=std::fs::read(&path).unwrap();let source=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let before=comments_png(&service,source.id,0,640);let snapshot=print_snapshot(&service,source.id,0);let original_print=call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap();
            let query=call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap();assert_eq!(query.status,"supported","{}",query.reason.unwrap_or_default());assert_eq!(query.fields[0].checked,Some(false));assert_eq!(query.fields[1].value,"Original");
            let patches=vec![crate::forms::FieldValue::Checkbox {field_id:query.fields[0].field_id.clone(),checked:true},crate::forms::FieldValue::Text {field_id:query.fields[1].field_id.clone(),value:"Checked gjpqy".into()}];let output_path=folder.path().join(format!("checked-{rotation}-{inherited}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(source.id,0,patches,output_path.clone(),reply)).unwrap();assert!(!output.document.dirty);assert_eq!(output.document.revision,0);let after=comments_png(&service,output.document.id,0,640);
            let source_point=|x:u32,y:u32| {let (nx,ny)=(x as f32/after.width() as f32,y as f32/after.height() as f32);match rotation {0=>(50.0+290.0*nx,760.0-210.0*ny),90=>(50.0+290.0*ny,550.0+210.0*nx),180=>(340.0-290.0*nx,550.0+210.0*ny),_=>(340.0-290.0*ny,760.0-210.0*nx)}};
            let mut check_changed=0;let mut on_ink=0;let mut off_ink=0;let mut outside=0;
            for (x,y,pixel) in after.enumerate_pixels() {let(sx,sy)=source_point(x,y);let check=(61.5..=78.5).contains(&sx)&&(651.5..=668.5).contains(&sy);let allowed=(58.0..=82.0).contains(&sx)&&(648.0..=672.0).contains(&sy)||(58.0..=312.0).contains(&sx)&&(578.0..=606.0).contains(&sy);if check {if pixel!=before.get_pixel(x,y) {check_changed+=1;}if pixel.0.iter().all(|value|*value<100) {on_ink+=1;}if before.get_pixel(x,y).0.iter().all(|value|*value<100) {off_ink+=1;}}if pixel!=before.get_pixel(x,y)&&!allowed {outside+=1;}}
            assert!(check_changed>50 && on_ink>50,"Independently positioned checked pixels {rotation}/{inherited}: changed {check_changed}, ink {on_ink}");assert_eq!(off_ink,0,"The unchecked interior must actually be empty");assert_eq!(outside,0,"Only checkbox and text widgets may change");
            let fields=call(&service,|reply|Request::FormFields(output.document.id,0,reply)).unwrap();assert_eq!(fields.fields[0].checked,Some(true));let off_path=folder.path().join(format!("off-{rotation}-{inherited}.pdf"));let cleared=call(&service,|reply|Request::FillFormCopy(output.document.id,0,vec![crate::forms::FieldValue::Checkbox {field_id:fields.fields[0].field_id.clone(),checked:false}],off_path.clone(),reply)).unwrap();let off=comments_png(&service,cleared.document.id,0,640);for(x,y,_)in off.enumerate_pixels(){let(sx,sy)=source_point(x,y);if(58.0..=82.0).contains(&sx)&&(648.0..=672.0).contains(&sy){assert_eq!(off.get_pixel(x,y),before.get_pixel(x,y),"Clearing removes the actual mark");}else{assert_eq!(off.get_pixel(x,y),after.get_pixel(x,y),"Clearing preserves the edited text and every other pixel");}}
            let unchanged_print=call(&service,|reply|Request::PrintRender(snapshot.token,0,640,640,reply)).unwrap();assert_eq!(unchanged_print.bgra,original_print.bgra);let on_snapshot=print_snapshot(&service,output.document.id,0);let off_snapshot=print_snapshot(&service,cleared.document.id,0);let on_print=call(&service,|reply|Request::PrintRender(on_snapshot.token,0,640,640,reply)).unwrap();let off_print=call(&service,|reply|Request::PrintRender(off_snapshot.token,0,640,640,reply)).unwrap();assert_ne!(on_print.bgra,off_print.bgra,"With equal text values, print bitmaps must differ because the checkbox is painted");
            let saved=lopdf::Document::load(&output_path).unwrap();for(id,object)in&doc.objects {if matches!(object,lopdf::Object::Stream(_)){assert_eq!(saved.get_object(*id).unwrap(),object,"Every original page/content/checkbox AP stream is retained");}}for key in[b"Rotate".as_slice(),b"CropBox"]{assert_eq!(doc.get_dictionary(owner).unwrap().get(key).unwrap(),saved.get_dictionary(owner).unwrap().get(key).unwrap());}
            assert_eq!(comments_png(&service,source.id,0,640),before);assert_eq!(std::fs::read(&path).unwrap(),bytes);let reopened=call(&service,|reply|Request::Open(output_path.clone(),reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,0,640),after);assert_eq!(call(&service,|reply|Request::FormFields(reopened.id,0,reply)).unwrap().fields[0].checked,Some(true));assert_eq!(call(&service,|reply|Request::FormFields(cleared.document.id,0,reply)).unwrap().fields[0].checked,Some(false));
            if rotation==0&&!inherited {let proof=root.join("target/forms-probe");std::fs::create_dir_all(&proof).unwrap();std::fs::write(proof.join("checkbox-cropped-source.pdf"),&bytes).unwrap();std::fs::copy(&output_path,proof.join("checkbox-filled.pdf")).unwrap();std::fs::copy(off_path,proof.join("checkbox-cleared.pdf")).unwrap();before.save(proof.join("checkbox-source-pdfium.png")).unwrap();after.save(proof.join("checkbox-filled-pdfium.png")).unwrap();off.save(proof.join("checkbox-cleared-pdfium.png")).unwrap();println!("Checkbox cross-engine outputs: {}",proof.display());}
            for id in[source.id,output.document.id,cleared.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
        }
    }
    #[test]
    fn checkboxes_mixed_corpora_preserve_every_original_page_and_source() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for(index,(fixture,count))in[("resources/welcome.pdf",6usize),("../test-corpus/synthetic-scan-98.pdf",98),("../test-corpus/synthetic-text-1500.pdf",1500)].into_iter().enumerate(){
            let source_path=root.join(fixture);let bytes=std::fs::read(&source_path).unwrap();let original=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let baseline=(0..count).map(|page|call(&service,|reply|Request::Render(original.id,page as u16,64,reply)).unwrap()).collect::<Vec<_>>();
            let mut combined=lopdf::Document::load_mem(&bytes).unwrap();let mut form=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf")).unwrap();form.renumber_objects_with(combined.max_id+1);let form_page=*form.get_pages().values().next().unwrap();let form_ref=form.catalog().unwrap().get(b"AcroForm").unwrap().clone();let root_id=combined.trailer.get(b"Root").unwrap().as_reference().unwrap();let pages_id=combined.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();form.get_dictionary_mut(form_page).unwrap().set("Parent",pages_id);combined.objects.extend(form.objects);combined.max_id=combined.objects.keys().map(|id|id.0).max().unwrap();combined.get_dictionary_mut(root_id).unwrap().set("AcroForm",form_ref);let pages=combined.get_dictionary_mut(pages_id).unwrap();pages.get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(lopdf::Object::Reference(form_page));pages.set("Count",(count+1)as i64);
            let input_path=folder.path().join(format!("mixed-corpus-{index}.pdf"));combined.save(&input_path).unwrap();let input_bytes=std::fs::read(&input_path).unwrap();let input=call(&service,|reply|Request::Open(input_path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());assert_eq!(fields.fields.len(),2);assert_eq!(fields.fields[0].checked,Some(false));assert!(fields.fields.iter().all(|field|field.page==count));let patches=fields.fields.iter().map(|field|if field.checked.is_some(){crate::forms::FieldValue::Checkbox {field_id:field.field_id.clone(),checked:true}}else{crate::forms::FieldValue::Text {field_id:field.field_id.clone(),value:"Mixed corpus".into()}}).collect();let output_path=folder.path().join(format!("checked-corpus-{index}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(input.id,0,patches,output_path.clone(),reply)).unwrap();assert_eq!(output.document.pages.len(),count+1);
            for(page,expected)in baseline.iter().enumerate(){assert_eq!(&call(&service,|reply|Request::Render(output.document.id,page as u16,64,reply)).unwrap(),expected,"Mixed corpus {index} original page {page}");assert_eq!(output.document.pages[page].width,original.pages[page].width);assert_eq!(output.document.pages[page].height,original.pages[page].height);}
            let source_form=comments_png(&service,input.id,count as u16,612);let filled_form=comments_png(&service,output.document.id,count as u16,612);let mut changed=0;let mut ink=0;for y in 123..141{for x in 61..79{if source_form.get_pixel(x,y)!=filled_form.get_pixel(x,y){changed+=1;}if filled_form.get_pixel(x,y).0.iter().all(|channel|*channel<100){ink+=1;}}}assert!(changed>30&&ink>30,"Independent checkbox region of appended form: {changed} changed pixels, {ink} dark pixels");let reopened=call(&service,|reply|Request::Open(output_path,reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,count as u16,612),filled_form);let readback=call(&service,|reply|Request::FormFields(reopened.id,0,reply)).unwrap();assert_eq!(readback.fields[0].checked,Some(true));assert_eq!(readback.fields[1].value,"Mixed corpus");assert_eq!(std::fs::read(&input_path).unwrap(),input_bytes);assert_eq!(std::fs::read(&source_path).unwrap(),bytes);assert_eq!(call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap().fields[0].checked,Some(false));for id in[original.id,input.id,output.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}println!("Checkbox corpus {fixture}: all {count} original page bitmaps at width 64 retained, mixed field page independently painted and reopened");
        }
    }
    #[test]
    fn choices_corpora_preserve_every_original_page_and_source() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for(index,(fixture,count))in[("resources/welcome.pdf",6usize),("../test-corpus/synthetic-scan-98.pdf",98),("../test-corpus/synthetic-text-1500.pdf",1500)].into_iter().enumerate(){
            let source_path=root.join(fixture);let bytes=std::fs::read(&source_path).unwrap();let original=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let baseline=(0..count).map(|page|call(&service,|reply|Request::Render(original.id,page as u16,64,reply)).unwrap()).collect::<Vec<_>>();
            let mut combined=lopdf::Document::load_mem(&bytes).unwrap();let mut form=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf")).unwrap();form.renumber_objects_with(combined.max_id+1);let form_page=*form.get_pages().values().next().unwrap();let form_ref=form.catalog().unwrap().get(b"AcroForm").unwrap().clone();let root_id=combined.trailer.get(b"Root").unwrap().as_reference().unwrap();let pages_id=combined.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();form.get_dictionary_mut(form_page).unwrap().set("Parent",pages_id);combined.objects.extend(form.objects);combined.max_id=combined.objects.keys().map(|id|id.0).max().unwrap();combined.get_dictionary_mut(root_id).unwrap().set("AcroForm",form_ref);let pages=combined.get_dictionary_mut(pages_id).unwrap();pages.get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(lopdf::Object::Reference(form_page));pages.set("Count",(count+1)as i64);
            let input_path=folder.path().join(format!("choice-corpus-{index}.pdf"));combined.save(&input_path).unwrap();let input_bytes=std::fs::read(&input_path).unwrap();let input=call(&service,|reply|Request::Open(input_path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());assert_eq!(fields.fields.len(),2);assert_eq!(fields.fields[0].value,"south-002");assert!(fields.fields.iter().all(|field|field.page==count));let patches=fields.fields.iter().map(|field|crate::forms::FieldValue::Choice {field_id:field.field_id.clone(),option_id:Some(field.choice.as_ref().unwrap().options[0].option_id.clone())}).collect();let output_path=folder.path().join(format!("choice-filled-corpus-{index}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(input.id,0,patches,output_path.clone(),reply)).unwrap();assert_eq!(output.document.pages.len(),count+1);
            for(page,expected)in baseline.iter().enumerate(){assert_eq!(&call(&service,|reply|Request::Render(output.document.id,page as u16,64,reply)).unwrap(),expected,"Mixed corpus {index} original page {page}");assert_eq!(output.document.pages[page].width,original.pages[page].width);assert_eq!(output.document.pages[page].height,original.pages[page].height);}
            let source_form=comments_png(&service,input.id,count as u16,612);let filled_form=comments_png(&service,output.document.id,count as u16,612);let mut changed=0;let mut ink=0;for y in 193..204{for x in 70..270{if source_form.get_pixel(x,y)!=filled_form.get_pixel(x,y){changed+=1;}let pixel=filled_form.get_pixel(x,y);if (140..=180).contains(&pixel.0[0])&&(175..=205).contains(&pixel.0[1])&&(200..=235).contains(&pixel.0[2]){ink+=1;}}}assert!(changed>1500&&ink>1500,"Independent choice list region of appended form: {changed} changed pixels, {ink} selected blue pixels");let reopened=call(&service,|reply|Request::Open(output_path,reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,count as u16,612),filled_form);let readback=call(&service,|reply|Request::FormFields(reopened.id,0,reply)).unwrap();assert_eq!(readback.fields[0].value,"north-001");assert_eq!(std::fs::read(&input_path).unwrap(),input_bytes);assert_eq!(std::fs::read(&source_path).unwrap(),bytes);assert_eq!(call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap().fields[0].value,"south-002");for id in[original.id,input.id,output.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}println!("Choice corpus {fixture}: all {count} original page bitmaps at width 64 retained, mixed field page independently painted and reopened");
        }
    }
    #[test]
    fn radios_corpora_preserve_every_original_page_and_source() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        for(index,(fixture,count))in[("resources/welcome.pdf",6usize),("../test-corpus/synthetic-scan-98.pdf",98),("../test-corpus/synthetic-text-1500.pdf",1500)].into_iter().enumerate(){
            let source_path=root.join(fixture);let bytes=std::fs::read(&source_path).unwrap();let original=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let baseline=(0..count).map(|page|call(&service,|reply|Request::Render(original.id,page as u16,64,reply)).unwrap()).collect::<Vec<_>>();
            let mut combined=lopdf::Document::load_mem(&bytes).unwrap();let mut form=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-radio-fields.pdf")).unwrap();form.renumber_objects_with(combined.max_id+1);let form_page=*form.get_pages().values().next().unwrap();let form_ref=form.catalog().unwrap().get(b"AcroForm").unwrap().clone();let root_id=combined.trailer.get(b"Root").unwrap().as_reference().unwrap();let pages_id=combined.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();form.get_dictionary_mut(form_page).unwrap().set("Parent",pages_id);combined.objects.extend(form.objects);combined.max_id=combined.objects.keys().map(|id|id.0).max().unwrap();combined.get_dictionary_mut(root_id).unwrap().set("AcroForm",form_ref);let pages=combined.get_dictionary_mut(pages_id).unwrap();pages.get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(lopdf::Object::Reference(form_page));pages.set("Count",(count+1)as i64);
            let input_path=folder.path().join(format!("radio-corpus-{index}.pdf"));combined.save(&input_path).unwrap();let input_bytes=std::fs::read(&input_path).unwrap();let input=call(&service,|reply|Request::Open(input_path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());assert_eq!(fields.fields.len(),1);assert_eq!(fields.fields[0].value,"Choice B");assert!(fields.fields.iter().all(|field|field.page==count));let patches=fields.fields.iter().map(|field|crate::forms::FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some(field.radio.as_ref().unwrap().options[0].option_id.clone())}).collect();let output_path=folder.path().join(format!("radio-filled-corpus-{index}.pdf"));let output=call(&service,|reply|Request::FillFormCopy(input.id,0,patches,output_path.clone(),reply)).unwrap();assert_eq!(output.document.pages.len(),count+1);
            for(page,expected)in baseline.iter().enumerate(){assert_eq!(&call(&service,|reply|Request::Render(output.document.id,page as u16,64,reply)).unwrap(),expected,"Mixed corpus {index} original page {page}");assert_eq!(output.document.pages[page].width,original.pages[page].width);assert_eq!(output.document.pages[page].height,original.pages[page].height);}
            let source_form=comments_png(&service,input.id,count as u16,612);let filled_form=comments_png(&service,output.document.id,count as u16,612);let mut changed=0;let mut ink=0;for y in 128..136{for x in 66..74{if source_form.get_pixel(x,y)!=filled_form.get_pixel(x,y){changed+=1;}if filled_form.get_pixel(x,y).0.iter().all(|channel|*channel<100){ink+=1;}}}assert!(changed>30&&ink>30,"Independent radio region of appended form: {changed} changed pixels, {ink} dark pixels");let reopened=call(&service,|reply|Request::Open(output_path,reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,count as u16,612),filled_form);let readback=call(&service,|reply|Request::FormFields(reopened.id,0,reply)).unwrap();assert_eq!(readback.fields[0].value,"Choice A");assert_eq!(std::fs::read(&input_path).unwrap(),input_bytes);assert_eq!(std::fs::read(&source_path).unwrap(),bytes);assert_eq!(call(&service,|reply|Request::FormFields(input.id,0,reply)).unwrap().fields[0].value,"Choice B");for id in[original.id,input.id,output.document.id,reopened.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}println!("Radio corpus {fixture}: all {count} original page bitmaps at width 64 retained, mixed field page independently painted and reopened");
        }
    }

    #[test]
    fn choices_clear_renders_blank_with_pdfium_and_radio_clear_refuses() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();
        let source_path=root.join("tests/fixtures/reportlab-choice-fields.pdf");let bytes=std::fs::read(&source_path).unwrap();let source=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let before=comments_png(&service,source.id,0,640);let query=call(&service,|reply|Request::FormFields(source.id,0,reply)).unwrap();assert_eq!(query.status,"supported","{}",query.reason.unwrap_or_default());assert!(query.fields.iter().all(|field|field.choice.as_ref().unwrap().selected_option_id.is_some()));
        let values=query.fields.iter().map(|field|crate::forms::FieldValue::Choice {field_id:field.field_id.clone(),option_id:None}).collect();let output_path=folder.path().join("choice-cleared.pdf");let output=call(&service,|reply|Request::FillFormCopy(source.id,0,values,output_path.clone(),reply)).unwrap();let after=comments_png(&service,output.document.id,0,640);assert_ne!(after,before);
        let readback=call(&service,|reply|Request::FormFields(output.document.id,0,reply)).unwrap();assert_eq!(readback.status,"supported");assert!(readback.fields.iter().all(|field|field.value.is_empty()&&field.choice.as_ref().unwrap().selected_option_id.is_none()));
        let saved=lopdf::Document::load(&output_path).unwrap();let form=saved.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();for reference in saved.get_dictionary(form).unwrap().get(b"Fields").unwrap().as_array().unwrap(){let field=saved.get_dictionary(reference.as_reference().unwrap()).unwrap();assert!(field.get(b"V").is_err()&&field.get(b"I").is_err());}
        let blank=call(&service,|reply|Request::Open(root.join("tests/fixtures/reportlab-choice-blank-fields.pdf"),reply)).unwrap();let expected=comments_png(&service,blank.id,0,640);assert_eq!(after.dimensions(),expected.dimensions());let differing=after.pixels().zip(expected.pixels()).filter(|(a,b)|a!=b).count();assert_eq!(differing,0,"A cleared choice copy must render like the source-blank ReportLab fixture under PDFium");
        assert_eq!(comments_png(&service,source.id,0,640),before);assert_eq!(std::fs::read(&source_path).unwrap(),bytes);let reopened=call(&service,|reply|Request::Open(output_path.clone(),reply)).unwrap();assert_eq!(comments_png(&service,reopened.id,0,640),after);
        if root.join("target/forms-probe").is_dir(){std::fs::copy(&output_path,root.join("target/forms-probe/choice-cleared.pdf")).unwrap();after.save(root.join("target/forms-probe/choice-cleared-pdfium.png")).unwrap();}
        let radio_path=root.join("tests/fixtures/reportlab-radio-fields.pdf");let radio=call(&service,|reply|Request::Open(radio_path,reply)).unwrap();let field=call(&service,|reply|Request::FormFields(radio.id,0,reply)).unwrap().fields[0].field_id.clone();let rejected=folder.path().join("radio-cleared.pdf");let error=call(&service,|reply|Request::FillFormCopy(radio.id,0,vec![crate::forms::FieldValue::Radio {field_id:field,option_id:None}],rejected.clone(),reply)).err().unwrap();assert!(error.contains("cannot be cleared"),"{error}");assert!(!rejected.exists());
        for id in[source.id,output.document.id,blank.id,reopened.id,radio.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
    }
    #[test]
    fn choices_copy_guards_preclosed_unread_and_accepted_ownership() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();let path=folder.path().join("choice-owned-source.pdf");let bytes=include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf");std::fs::write(&path,bytes).unwrap();let info=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());let field=&fields.fields[0];let choice=field.choice.as_ref().unwrap();let values=vec![crate::forms::FieldValue::Choice {field_id:field.field_id.clone(),option_id:Some(choice.options[0].option_id.clone())}];
        let rejected=folder.path().join("rejected.pdf");for patches in[vec![crate::forms::FieldValue::Text {field_id:fields.fields[0].field_id.clone(),value:"false".into()}],vec![crate::forms::FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some("unknown".into())}],vec![crate::forms::FieldValue::Checkbox {field_id:"unknown".into(),checked:false}],vec![values[0].clone(),values[0].clone()]]{assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,patches,rejected.clone(),reply)).is_err());assert!(!rejected.exists());}
        assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),path.clone(),reply)).is_err());let existing=folder.path().join("existing.pdf");std::fs::write(&existing,b"existing retained").unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),existing.clone(),reply)).is_err());assert_eq!(std::fs::read(existing).unwrap(),b"existing retained");
        let preclosed=folder.path().join("preclosed.pdf");let(tx,rx)=oneshot::channel();drop(rx);service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),preclosed.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(!preclosed.exists());let unread=folder.path().join("unread.pdf");let(tx,rx)=oneshot::channel();service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),unread.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(unread.exists());drop(rx);assert!(call(&service,|reply|Request::OpenDocumentsForPath(unread.clone(),reply)).unwrap().is_empty());let output=call(&service,|reply|Request::Open(unread,reply)).unwrap();assert_eq!(call(&service,|reply|Request::FormFields(output.id,0,reply)).unwrap().fields[0].value,"north-001");call(&service,|reply|Request::Close(output.id,reply)).unwrap();
        let accepted=tauri::async_runtime::block_on(service.fill_form_copy(info.id,0,values.clone(),folder.path().join("accepted.pdf"))).unwrap();let accepted_id=accepted.document.id;drop(accepted);assert_eq!(call(&service,|reply|Request::FormFields(accepted_id,0,reply)).unwrap().fields[0].value,"north-001");call(&service,|reply|Request::Close(accepted_id,reply)).unwrap();
        let edited=call(&service,|reply|Request::Edit(info.id,PageEdit::Rotate {pages:vec![0],clockwise:true},reply)).unwrap();assert!(call(&service,|reply|Request::FormFields(info.id,0,reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),rejected.clone(),reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,edited.revision,values.clone(),rejected.clone(),reply)).is_err());let undone=call(&service,|reply|Request::Edit(info.id,PageEdit::Undo,reply)).unwrap();assert!(undone.can_redo);let filled=call(&service,|reply|Request::FillFormCopy(info.id,undone.revision,values.clone(),folder.path().join("after-undo.pdf"),reply)).unwrap();let redone=call(&service,|reply|Request::Edit(info.id,PageEdit::Redo,reply)).unwrap();assert!(redone.dirty);assert_eq!(std::fs::read(&path).unwrap(),bytes);call(&service,|reply|Request::Close(filled.document.id,reply)).unwrap();call(&service,|reply|Request::Close(info.id,reply)).unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,redone.revision,values,rejected.clone(),reply)).is_err());assert!(!rejected.exists());
    }
    #[test]
    fn radios_copy_guards_preclosed_unread_and_accepted_ownership() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();let path=folder.path().join("radio-owned-source.pdf");let bytes=include_bytes!("../tests/fixtures/reportlab-radio-fields.pdf");std::fs::write(&path,bytes).unwrap();let info=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());let field=&fields.fields[0];let radio=field.radio.as_ref().unwrap();let values=vec![crate::forms::FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some(radio.options[0].option_id.clone())}];
        let rejected=folder.path().join("rejected.pdf");for patches in[vec![crate::forms::FieldValue::Text {field_id:fields.fields[0].field_id.clone(),value:"false".into()}],vec![crate::forms::FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some("unknown".into())}],vec![crate::forms::FieldValue::Checkbox {field_id:"unknown".into(),checked:false}],vec![values[0].clone(),values[0].clone()]]{assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,patches,rejected.clone(),reply)).is_err());assert!(!rejected.exists());}
        assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),path.clone(),reply)).is_err());let existing=folder.path().join("existing.pdf");std::fs::write(&existing,b"existing retained").unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),existing.clone(),reply)).is_err());assert_eq!(std::fs::read(existing).unwrap(),b"existing retained");
        let preclosed=folder.path().join("preclosed.pdf");let(tx,rx)=oneshot::channel();drop(rx);service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),preclosed.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(!preclosed.exists());let unread=folder.path().join("unread.pdf");let(tx,rx)=oneshot::channel();service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),unread.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(unread.exists());drop(rx);assert!(call(&service,|reply|Request::OpenDocumentsForPath(unread.clone(),reply)).unwrap().is_empty());let output=call(&service,|reply|Request::Open(unread,reply)).unwrap();assert_eq!(call(&service,|reply|Request::FormFields(output.id,0,reply)).unwrap().fields[0].value,"Choice A");call(&service,|reply|Request::Close(output.id,reply)).unwrap();
        let accepted=tauri::async_runtime::block_on(service.fill_form_copy(info.id,0,values.clone(),folder.path().join("accepted.pdf"))).unwrap();let accepted_id=accepted.document.id;drop(accepted);assert_eq!(call(&service,|reply|Request::FormFields(accepted_id,0,reply)).unwrap().fields[0].value,"Choice A");call(&service,|reply|Request::Close(accepted_id,reply)).unwrap();
        let edited=call(&service,|reply|Request::Edit(info.id,PageEdit::Rotate {pages:vec![0],clockwise:true},reply)).unwrap();assert!(call(&service,|reply|Request::FormFields(info.id,0,reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),rejected.clone(),reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,edited.revision,values.clone(),rejected.clone(),reply)).is_err());let undone=call(&service,|reply|Request::Edit(info.id,PageEdit::Undo,reply)).unwrap();assert!(undone.can_redo);let filled=call(&service,|reply|Request::FillFormCopy(info.id,undone.revision,values.clone(),folder.path().join("after-undo.pdf"),reply)).unwrap();let redone=call(&service,|reply|Request::Edit(info.id,PageEdit::Redo,reply)).unwrap();assert!(redone.dirty);assert_eq!(std::fs::read(&path).unwrap(),bytes);call(&service,|reply|Request::Close(filled.document.id,reply)).unwrap();call(&service,|reply|Request::Close(info.id,reply)).unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,redone.revision,values,rejected.clone(),reply)).is_err());assert!(!rejected.exists());
    }
    #[test]
    fn checkboxes_custom_checked_source_copy_guards_and_reply_ownership() {
        let root=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let service=PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();let mut doc=lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf")).unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let checkbox=doc.get_dictionary(form).unwrap().get(b"Fields").unwrap().as_array().unwrap()[0].as_reference().unwrap();let widget=doc.get_dictionary_mut(checkbox).unwrap();for(_,states)in widget.get_mut(b"AP").unwrap().as_dict_mut().unwrap(){let states=states.as_dict_mut().unwrap();let on=states.remove(b"Yes").unwrap();states.set("Approved",on);}widget.set("V",lopdf::Object::Name(b"Approved".to_vec()));widget.set("AS",lopdf::Object::Name(b"Approved".to_vec()));widget.set("DV",lopdf::Object::Name(b"Off".to_vec()));let path=folder.path().join("custom-checked-source.pdf");doc.save(&path).unwrap();let bytes=std::fs::read(&path).unwrap();let info=call(&service,|reply|Request::Open(path.clone(),reply)).unwrap();let fields=call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert_eq!(fields.status,"supported","{}",fields.reason.unwrap_or_default());assert_eq!(fields.fields[0].checked,Some(true));let values=vec![crate::forms::FieldValue::Checkbox {field_id:fields.fields[0].field_id.clone(),checked:false}];
        let rejected=folder.path().join("rejected.pdf");for patches in[vec![crate::forms::FieldValue::Text {field_id:fields.fields[0].field_id.clone(),value:"false".into()}],vec![crate::forms::FieldValue::Checkbox {field_id:fields.fields[1].field_id.clone(),checked:false}],vec![crate::forms::FieldValue::Checkbox {field_id:"unknown".into(),checked:false}],vec![values[0].clone(),values[0].clone()]]{assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,patches,rejected.clone(),reply)).is_err());assert!(!rejected.exists());}
        assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),path.clone(),reply)).is_err());let existing=folder.path().join("existing.pdf");std::fs::write(&existing,b"existing retained").unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),existing.clone(),reply)).is_err());assert_eq!(std::fs::read(existing).unwrap(),b"existing retained");
        let preclosed=folder.path().join("preclosed.pdf");let(tx,rx)=oneshot::channel();drop(rx);service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),preclosed.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(!preclosed.exists());let unread=folder.path().join("unread.pdf");let(tx,rx)=oneshot::channel();service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),unread.clone(),tx)).unwrap();call(&service,|reply|Request::FormFields(info.id,0,reply)).unwrap();assert!(unread.exists());drop(rx);assert!(call(&service,|reply|Request::OpenDocumentsForPath(unread.clone(),reply)).unwrap().is_empty());let output=call(&service,|reply|Request::Open(unread,reply)).unwrap();assert_eq!(call(&service,|reply|Request::FormFields(output.id,0,reply)).unwrap().fields[0].checked,Some(false));call(&service,|reply|Request::Close(output.id,reply)).unwrap();
        let accepted=tauri::async_runtime::block_on(service.fill_form_copy(info.id,0,values.clone(),folder.path().join("accepted.pdf"))).unwrap();let accepted_id=accepted.document.id;drop(accepted);assert_eq!(call(&service,|reply|Request::FormFields(accepted_id,0,reply)).unwrap().fields[0].checked,Some(false));call(&service,|reply|Request::Close(accepted_id,reply)).unwrap();
        let edited=call(&service,|reply|Request::Edit(info.id,PageEdit::Rotate {pages:vec![0],clockwise:true},reply)).unwrap();assert!(call(&service,|reply|Request::FormFields(info.id,0,reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,0,values.clone(),rejected.clone(),reply)).is_err());assert!(call(&service,|reply|Request::FillFormCopy(info.id,edited.revision,values.clone(),rejected.clone(),reply)).is_err());let undone=call(&service,|reply|Request::Edit(info.id,PageEdit::Undo,reply)).unwrap();assert!(undone.can_redo);let filled=call(&service,|reply|Request::FillFormCopy(info.id,undone.revision,values.clone(),folder.path().join("after-undo.pdf"),reply)).unwrap();let redone=call(&service,|reply|Request::Edit(info.id,PageEdit::Redo,reply)).unwrap();assert!(redone.dirty);assert_eq!(std::fs::read(&path).unwrap(),bytes);call(&service,|reply|Request::Close(filled.document.id,reply)).unwrap();call(&service,|reply|Request::Close(info.id,reply)).unwrap();assert!(call(&service,|reply|Request::FillFormCopy(info.id,redone.revision,values,rejected.clone(),reply)).is_err());assert!(!rejected.exists());
    }
    #[test]
    fn forms_reportlab_engine_crops_rotations_values_print_and_source_preservation() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for (rotation, inherited) in [0, 90, 180, 270].into_iter().flat_map(|rotation| [false,true].map(|inherited| (rotation,inherited))) {
            let mut document = lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-plain-fields.pdf")).unwrap();
            let page = *document.get_pages().values().next().unwrap(); let owner = if inherited { document.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap() } else {page};
            if inherited { let leaf = document.get_dictionary_mut(page).unwrap(); leaf.remove(b"Rotate"); leaf.remove(b"CropBox"); }
            let page_dict = document.get_dictionary_mut(owner).unwrap();
            page_dict.set("Rotate", rotation); page_dict.set("CropBox", vec![30.into(),500.into(),350.into(),760.into()]);
            let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); let source_path = folder.path().join(format!("source-{rotation}-{inherited}.pdf")); std::fs::write(&source_path, &bytes).unwrap();
            let source = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap(); let before = comments_png(&service, source.id, 0, 640); let snapshot = print_snapshot(&service, source.id, 0);
            let before_print = call(&service, |reply| Request::PrintRender(snapshot.token, 0, 640, 640, reply)).unwrap();
            let query = call(&service, |reply| Request::FormFields(source.id, 0, reply)).unwrap(); assert_eq!(query.status, "supported"); assert_eq!(query.fields.len(), 2); assert_eq!(query.fields[0].page, 0);
            let values = query.fields.iter().enumerate().map(|(index, field)| crate::forms::FieldValue::Text { field_id:field.field_id.clone(), value: if index == 0 { "Changed gjpqy".into() } else { "Portland".into() } }).collect::<Vec<_>>();
            call(&service, |reply| Request::CheckFormCopy(source.id, 0, values.clone(), reply)).unwrap();
            let output_path = folder.path().join(format!("filled-{rotation}-{inherited}.pdf")); let output = call(&service, |reply| Request::FillFormCopy(source.id, 0, values, output_path.clone(), reply)).unwrap();
            assert_eq!(output.document.revision, 0); assert!(!output.document.dirty); assert!(!output.document.can_undo); assert_eq!(output.document.pages[0].width, source.pages[0].width); assert_eq!(output.document.pages[0].height, source.pages[0].height);
            let after = comments_png(&service, output.document.id, 0, 640); assert_eq!(after.dimensions(), before.dimensions());
            let mut changed = 0; let mut outside_changed = 0; let mut ink = 0;
            for (x,y,pixel) in after.enumerate_pixels() {
                let (nx,ny) = (x as f32 / after.width() as f32, y as f32 / after.height() as f32);
                let (sx,sy) = match rotation { 0 => (30.0+320.0*nx,760.0-260.0*ny),90 => (30.0+320.0*ny,500.0+260.0*nx),180 => (350.0-320.0*nx,500.0+260.0*ny),_ => (350.0-320.0*ny,760.0-260.0*nx) };
                let field = (58.0..=312.0).contains(&sx) && ((648.0..=676.0).contains(&sy) || (568.0..=598.0).contains(&sy));
                if pixel != before.get_pixel(x,y) { changed += 1; if !field { outside_changed += 1; } }
                if field && pixel.0.iter().all(|channel| *channel < 120) { ink += 1; }
            }
            assert!(changed > 300 && ink > 100, "Independent visible AP pixels for rotation {rotation}: changed {changed}, ink {ink}"); assert_eq!(outside_changed, 0, "Only field interiors may change");
            assert_eq!(comments_png(&service, source.id, 0, 640), before); assert_eq!(std::fs::read(&source_path).unwrap(), bytes);
            let still_old_print = call(&service, |reply| Request::PrintRender(snapshot.token, 0, 640, 640, reply)).unwrap(); assert_eq!(still_old_print.bgra, before_print.bgra);
            let new_snapshot = print_snapshot(&service, output.document.id, 0); let filled_print = call(&service, |reply| Request::PrintRender(new_snapshot.token, 0, 640, 640, reply)).unwrap(); assert_ne!(filled_print.bgra, before_print.bgra);
            let saved_bytes = std::fs::read(&output_path).unwrap(); let saved = lopdf::Document::load_mem(&saved_bytes).unwrap(); let saved_page = *saved.get_pages().values().next().unwrap(); for key in [b"Rotate".as_slice(), b"CropBox"] { assert_eq!(document.get_dictionary(owner).unwrap().get(key).unwrap(), saved.get_dictionary(owner).unwrap().get(key).unwrap()); } for key in [b"Contents".as_slice(), b"Resources"] { assert_eq!(document.get_dictionary(page).unwrap().get(key).unwrap(), saved.get_dictionary(saved_page).unwrap().get(key).unwrap()); }
            let reopened = call(&service, |reply| Request::Open(output_path, reply)).unwrap(); assert_eq!(comments_png(&service, reopened.id, 0, 640), after); let readback = call(&service, |reply| Request::FormFields(reopened.id, 0, reply)).unwrap(); assert_eq!(readback.fields[0].value, "Changed gjpqy"); assert_eq!(readback.fields[1].value, "Portland");
            if rotation == 0 { let proof = root.join("target/forms-probe"); std::fs::create_dir_all(&proof).unwrap(); std::fs::write(proof.join("filled-reportlab.pdf"), &saved_bytes).unwrap(); after.save(proof.join("filled-reportlab-pdfium.png")).unwrap(); std::fs::write(proof.join("cropped-reportlab-source.pdf"), &bytes).unwrap(); println!("Independent cross-engine output: {}", proof.join("filled-reportlab.pdf").display()); }
            if rotation == 0 && !inherited {
                let fields = call(&service, |reply| Request::FormFields(output.document.id,0,reply)).unwrap(); let clear = vec![crate::forms::FieldValue::Text {field_id:fields.fields[0].field_id.clone(),value:String::new()}]; let cleared = call(&service, |reply| Request::FillFormCopy(output.document.id,0,clear,folder.path().join("cleared.pdf"),reply)).unwrap(); let empty = comments_png(&service,cleared.document.id,0,640);
                assert!((176..216).all(|y| (64..556).all(|x| empty.get_pixel(x,y).0 == [255,255,255])), "Clearing canonical V must actually remove painted text, retaining white background"); assert_eq!(call(&service, |reply| Request::FormFields(cleared.document.id,0,reply)).unwrap().fields[0].value,""); assert_eq!(call(&service, |reply| Request::FormFields(cleared.document.id,0,reply)).unwrap().fields[1].value,"Portland"); call(&service, |reply| Request::Close(cleared.document.id,reply)).unwrap();
            }
            for id in [source.id,output.document.id,reopened.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
        }
    }
    #[test]
    fn forms_corpus_no_fields_and_appended_independent_form_preserve_every_original_page() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for (index, (fixture, count)) in [("resources/welcome.pdf",6usize),("../test-corpus/synthetic-scan-98.pdf",98),("../test-corpus/synthetic-text-1500.pdf",1500)].into_iter().enumerate() {
            let original_path = root.join(fixture); let source_bytes = std::fs::read(&original_path).unwrap(); let original = call(&service, |reply| Request::Open(original_path.clone(), reply)).unwrap();
            let query = call(&service, |reply| Request::FormFields(original.id, 0, reply)).unwrap(); assert_eq!(query.status,"supported"); assert!(query.fields.is_empty()); let absent_output = folder.path().join(format!("no-fields-{index}.pdf")); assert!(call(&service, |reply| Request::FillFormCopy(original.id, 0, vec![crate::forms::FieldValue::Text {field_id:"unknown".into(),value:"new".into()}], absent_output.clone(), reply)).is_err()); assert!(!absent_output.exists());
            let baseline = (0..count).map(|page| call(&service, |reply| Request::Render(original.id,page as u16,64,reply)).unwrap()).collect::<Vec<_>>();
            let mut combined = lopdf::Document::load_mem(&source_bytes).unwrap(); let mut form = lopdf::Document::load_mem(include_bytes!("../tests/fixtures/reportlab-plain-fields.pdf")).unwrap(); form.renumber_objects_with(combined.max_id+1);
            let form_page = *form.get_pages().values().next().unwrap(); let form_ref = form.catalog().unwrap().get(b"AcroForm").unwrap().clone(); let root_id = combined.trailer.get(b"Root").unwrap().as_reference().unwrap(); let pages_id = combined.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();
            form.get_dictionary_mut(form_page).unwrap().set("Parent",pages_id); combined.objects.extend(form.objects); combined.max_id = combined.objects.keys().map(|id| id.0).max().unwrap(); combined.get_dictionary_mut(root_id).unwrap().set("AcroForm",form_ref); let page_tree = combined.get_dictionary_mut(pages_id).unwrap(); page_tree.get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(lopdf::Object::Reference(form_page)); page_tree.set("Count",(count+1) as i64);
            let appended = folder.path().join(format!("appended-{index}.pdf")); combined.save(&appended).unwrap(); let input_bytes = std::fs::read(&appended).unwrap(); let input = call(&service, |reply| Request::Open(appended.clone(), reply)).unwrap(); let query = call(&service, |reply| Request::FormFields(input.id,0,reply)).unwrap(); assert_eq!(query.status,"supported", "{}", query.reason.unwrap_or_default()); assert_eq!(query.fields.len(),2); assert!(query.fields.iter().all(|field| field.page == count));
            let values = query.fields.iter().map(|field| crate::forms::FieldValue::Text {field_id:field.field_id.clone(),value:if field.name=="Name" {"Corpus copy".into()} else {"Portland".into()}}).collect(); let output_path = folder.path().join(format!("filled-corpus-{index}.pdf")); let output = call(&service, |reply| Request::FillFormCopy(input.id,0,values,output_path.clone(),reply)).unwrap(); assert_eq!(output.document.pages.len(),count+1);
            for (page, expected) in baseline.iter().enumerate() { let actual = call(&service, |reply| Request::Render(output.document.id,page as u16,64,reply)).unwrap(); assert_eq!(&actual,expected,"Preserve original corpus {index} page {page}"); assert_eq!(original.pages[page].width,output.document.pages[page].width); assert_eq!(original.pages[page].height,output.document.pages[page].height); }
            assert_ne!(comments_png(&service,input.id,count as u16,612),comments_png(&service,output.document.id,count as u16,612)); assert_eq!(std::fs::read(&original_path).unwrap(),source_bytes); assert_eq!(std::fs::read(&appended).unwrap(),input_bytes); let unchanged = call(&service, |reply| Request::FormFields(input.id,0,reply)).unwrap(); assert_eq!(unchanged.fields[0].value,"Original");
            let reopened = call(&service, |reply| Request::Open(output_path,reply)).unwrap(); assert_eq!(comments_png(&service,reopened.id,count as u16,612),comments_png(&service,output.document.id,count as u16,612));
            for id in [original.id,input.id,output.document.id,reopened.id] { call(&service, |reply| Request::Close(id,reply)).unwrap(); }
            println!("Form corpus {fixture}: {count} original pages compared at width 64 plus independently generated filled form page");
        }
    }
    #[test]
    fn forms_copy_guards_receiver_ownership_accepted_session_and_hidden_widgets() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap(); let path = root.join("tests/fixtures/reportlab-plain-fields.pdf"); let bytes = std::fs::read(&path).unwrap(); let info = call(&service, |reply| Request::Open(path.clone(),reply)).unwrap(); let query = call(&service, |reply| Request::FormFields(info.id,0,reply)).unwrap(); let values = vec![crate::forms::FieldValue::Text {field_id:query.fields[0].field_id.clone(),value:"Owned copy".into()}];
        assert!(call(&service, |reply| Request::FillFormCopy(info.id,0,values.clone(),path.clone(),reply)).is_err()); assert_eq!(std::fs::read(&path).unwrap(),bytes);
        let existing = folder.path().join("existing.pdf"); std::fs::write(&existing,b"keep existing").unwrap(); assert!(call(&service, |reply| Request::FillFormCopy(info.id,0,values.clone(),existing.clone(),reply)).is_err()); assert_eq!(std::fs::read(&existing).unwrap(),b"keep existing");
        let preclosed = folder.path().join("preclosed.pdf"); let (tx,rx) = oneshot::channel(); drop(rx); service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),preclosed.clone(),tx)).unwrap(); call(&service, |reply| Request::FormFields(info.id,0,reply)).unwrap(); assert!(!preclosed.exists());
        let unread = folder.path().join("unread.pdf"); let (tx,rx) = oneshot::channel(); service.sender.send(Request::FillFormCopy(info.id,0,values.clone(),unread.clone(),tx)).unwrap(); call(&service, |reply| Request::FormFields(info.id,0,reply)).unwrap(); assert!(unread.exists()); drop(rx); assert!(call(&service, |reply| Request::OpenDocumentsForPath(unread.clone(),reply)).unwrap().is_empty()); let reopened = call(&service, |reply| Request::Open(unread,reply)).unwrap(); call(&service, |reply| Request::Close(reopened.id,reply)).unwrap();
        let accepted_path = folder.path().join("accepted.pdf"); let accepted = tauri::async_runtime::block_on(service.fill_form_copy(info.id,0,values.clone(),accepted_path)).unwrap(); let accepted_id=accepted.document.id; drop(accepted); assert_eq!(call(&service, |reply| Request::FormFields(accepted_id,0,reply)).unwrap().fields[0].value,"Owned copy"); call(&service, |reply| Request::Close(accepted_id,reply)).unwrap();
        let rotated = call(&service, |reply| Request::Edit(info.id,PageEdit::Rotate {pages:vec![0],clockwise:true},reply)).unwrap(); assert!(call(&service, |reply| Request::FormFields(info.id,0,reply)).is_err()); assert_eq!(call(&service, |reply| Request::FormFields(info.id,rotated.revision,reply)).unwrap().status,"unsupported"); let changed_output = folder.path().join("changed.pdf"); assert!(call(&service, |reply| Request::FillFormCopy(info.id,0,values.clone(),changed_output.clone(),reply)).is_err()); assert!(call(&service, |reply| Request::FillFormCopy(info.id,rotated.revision,values.clone(),changed_output.clone(),reply)).is_err()); assert!(!changed_output.exists()); let undone = call(&service, |reply| Request::Edit(info.id,PageEdit::Undo,reply)).unwrap(); assert!(!undone.dirty); assert!(undone.can_redo); let fresh = call(&service, |reply| Request::FormFields(info.id,undone.revision,reply)).unwrap(); assert_eq!(fresh.status,"supported"); let undo_output = call(&service, |reply| Request::FillFormCopy(info.id,undone.revision,values,folder.path().join("after-undo.pdf"),reply)).unwrap(); assert_eq!(call(&service, |reply| Request::FormFields(info.id,undone.revision,reply)).unwrap().fields[0].value,"Original"); let redone = call(&service, |reply| Request::Edit(info.id,PageEdit::Redo,reply)).unwrap(); assert!(redone.dirty); call(&service, |reply| Request::Close(undo_output.document.id,reply)).unwrap(); call(&service, |reply| Request::Close(info.id,reply)).unwrap(); assert!(call(&service, |reply| Request::FormFields(info.id,redone.revision,reply)).is_err()); assert!(call(&service, |reply| Request::CheckFormCopy(info.id,redone.revision,vec![],reply)).is_err());
        let mut hidden = lopdf::Document::load_mem(&bytes).unwrap(); let page=*hidden.get_pages().values().next().unwrap(); hidden.get_dictionary_mut(page).unwrap().set("CropBox",vec![30.into(),680.into(),350.into(),760.into()]); let hidden_path=folder.path().join("hidden-source.pdf"); hidden.save(&hidden_path).unwrap(); let source=call(&service, |reply| Request::Open(hidden_path.clone(),reply)).unwrap(); let before=comments_png(&service,source.id,0,640); let fields=call(&service, |reply| Request::FormFields(source.id,0,reply)).unwrap(); assert_eq!(fields.status,"supported"); let patches=fields.fields.iter().map(|field| crate::forms::FieldValue::Text {field_id:field.field_id.clone(),value:"Hidden stored".into()}).collect(); let output=call(&service, |reply| Request::FillFormCopy(source.id,0,patches,folder.path().join("hidden-filled.pdf"),reply)).unwrap(); assert_eq!(comments_png(&service,output.document.id,0,640),before); assert!(call(&service, |reply| Request::FormFields(output.document.id,0,reply)).unwrap().fields.iter().all(|field| field.value=="Hidden stored")); for id in [source.id,output.document.id] {call(&service, |reply| Request::Close(id,reply)).unwrap();}
    }
    struct TestPrintSnapshot { service: PdfService, info: PrintSnapshotInfo }
    impl std::ops::Deref for TestPrintSnapshot {
        type Target = PrintSnapshotInfo;
        fn deref(&self) -> &Self::Target { &self.info }
    }
    impl Drop for TestPrintSnapshot {
        fn drop(&mut self) {
            let (tx, rx) = oneshot::channel();
            if self.service.sender.send(Request::EndPrint(self.info.token, tx)).is_ok() { let _ = rx.blocking_recv(); }
        }
    }
    fn print_snapshot(service: &PdfService, id: u64, revision: u64) -> TestPrintSnapshot {
        TestPrintSnapshot { service: service.clone(), info: call(service, |reply| Request::BeginPrint(id, revision, reply)).unwrap() }
    }
    fn comments_png(service: &PdfService, id: u64, page: u16, width: i32) -> image::RgbImage {
        image::load_from_memory(&call(service, |reply| Request::Render(id, page, width, reply)).unwrap()).unwrap().into_rgb8()
    }
    fn comments_yellow_bounds(image: &image::RgbImage) -> Option<(u32, u32, u32, u32)> {
        let mut pixels = image.enumerate_pixels().filter(|(_, _, pixel)| pixel[0] > 220 && pixel[1] > 150 && pixel[1] < 240 && pixel[2] < 60);
        let (x, y, _) = pixels.next()?; let mut bounds = (x, y, x, y);
        for (x, y, _) in pixels { bounds.0 = bounds.0.min(x); bounds.1 = bounds.1.min(y); bounds.2 = bounds.2.max(x); bounds.3 = bounds.3.max(y); }
        Some(bounds)
    }
    fn comments_assert_box(image: &image::RgbImage, rect: CropRect) {
        let bounds = comments_yellow_bounds(image).expect("The actual PDF bitmap must contain the note marker");
        for (actual, expected) in [(bounds.0 as f64, rect.x * image.width() as f64), (bounds.1 as f64, rect.y * image.height() as f64), ((bounds.2 + 1) as f64, (rect.x + rect.width) * image.width() as f64), ((bounds.3 + 1) as f64, (rect.y + rect.height) * image.height() as f64)] {
            assert!((actual - expected).abs() <= 2.0, "Actual yellow pixel boundary {actual}, independent expected boundary {expected}");
        }
    }
    fn comments_print_rgb(bitmap: &PrintBitmap) -> image::RgbImage {
        image::RgbImage::from_fn(bitmap.width, bitmap.height, |x, y| { let index = (y as usize * bitmap.width as usize + x as usize) * 4; image::Rgb([bitmap.bgra[index + 2], bitmap.bgra[index + 1], bitmap.bgra[index]]) })
    }
    fn highlights_assert_pixels(before: &image::RgbImage, after: &image::RgbImage, rect: CropRect, ink_threshold: u8) {
        assert_eq!(before.dimensions(), after.dimensions()); let mut tinted = 0; let mut dark = 0;
        for (x, y, pixel) in after.enumerate_pixels() {
            let old = before.get_pixel(x, y);
            let inside = x as f64 > rect.x * after.width() as f64 + 2.0 && (x as f64) < (rect.x + rect.width) * after.width() as f64 - 2.0 && y as f64 > rect.y * after.height() as f64 + 2.0 && (y as f64) < (rect.y + rect.height) * after.height() as f64 - 2.0;
            let outside = (x as f64) < rect.x * after.width() as f64 - 2.0 || (x as f64) > (rect.x + rect.width) * after.width() as f64 + 2.0 || (y as f64) < rect.y * after.height() as f64 - 2.0 || (y as f64) > (rect.y + rect.height) * after.height() as f64 + 2.0;
            if outside { assert_eq!(pixel, old, "An area highlight must not change unrelated pixels"); }
            if inside {
                assert!((pixel[0] as i16 - old[0] as i16).abs() <= 1 && (pixel[1] as i16 - old[1] as i16).abs() <= 1);
                assert!((pixel[2] as f64 - old[2] as f64 * 0.75).abs() <= 2.0, "Expected one yellow multiply fill at 25% opacity: old {old:?}, actual {pixel:?}");
                if old[0] > 240 && old[1] > 240 && old[2] > 240 { tinted += 1; }
                if old.0.iter().all(|value| *value < ink_threshold) { assert!(pixel.0.iter().all(|value| *value <= ink_threshold), "Highlight must keep fixture ink visible"); dark += 1; }
            }
        }
        assert!(tinted > 100, "The actual bitmap must show the area highlight"); assert!(dark > 0, "Fixture must independently place ink below {ink_threshold} inside the highlight");
    }
    fn highlights_assert_extent(image: &image::RgbImage, rect: CropRect) {
        let mut pixels = image.enumerate_pixels().filter(|(_, _, pixel)| pixel[0] > 240 && pixel[1] > 240 && (180..205).contains(&pixel[2])); let (x, y, _) = pixels.next().expect("Expected actual translucent highlight pixels"); let mut bounds = (x, y, x, y);
        for (x, y, _) in pixels { bounds.0 = bounds.0.min(x); bounds.1 = bounds.1.min(y); bounds.2 = bounds.2.max(x); bounds.3 = bounds.3.max(y); }
        for (actual, expected) in [(bounds.0 as f64, rect.x * image.width() as f64), (bounds.1 as f64, rect.y * image.height() as f64), ((bounds.2 + 1) as f64, (rect.x + rect.width) * image.width() as f64), ((bounds.3 + 1) as f64, (rect.y + rect.height) * image.height() as f64)] { assert!((actual - expected).abs() <= 2.0, "Actual highlight pixel extent {actual}, independently expected {expected}"); }
    }
    fn text_highlights_assert_pixels(before: &image::RgbImage, after: &image::RgbImage, quads: &[crate::comments::DisplayRect], ink_threshold: u8) {
        assert_eq!(before.dimensions(), after.dimensions()); let mut tinted = 0; let mut dark = 0; let mut outside = 0;
        let inside = |x: u32, y: u32, rect: &crate::comments::DisplayRect, margin: f64| x as f64 >= rect.x * before.width() as f64 + margin && x as f64 <= (rect.x + rect.width) * before.width() as f64 - margin && y as f64 >= rect.y * before.height() as f64 + margin && y as f64 <= (rect.y + rect.height) * before.height() as f64 - margin;
        for (x, y, pixel) in after.enumerate_pixels() {
            let old = before.get_pixel(x, y);
            if quads.iter().any(|rect| inside(x, y, rect, 2.0)) {
                assert!((pixel[0] as i16 - old[0] as i16).abs() <= 1 && (pixel[1] as i16 - old[1] as i16).abs() <= 1);
                assert!((pixel[2] as f64 - old[2] as f64 * 0.75).abs() <= 2.0, "Text-highlight quads must use one multiply fill even where they overlap: old {old:?}, after {pixel:?}");
                if old.0.iter().all(|value| *value > 240) { tinted += 1; }
                if old.0.iter().all(|value| *value < ink_threshold) { dark += 1; }
            } else if !quads.iter().any(|rect| inside(x, y, rect, -3.0)) { assert_eq!(pixel, old, "Pixels outside individual glyph quads, including line gaps, must remain unchanged"); outside += 1; }
        }
        assert!(tinted > 100 && outside > 100, "Insufficient independent pixel samples at {:?}: tinted {tinted}, outside {outside}, quads {}", before.dimensions(), quads.len()); assert!(dark > 0, "Independent baseline bitmap must contain glyph ink below {ink_threshold} under highlight quads");
    }
    #[test]
    fn text_highlights_multiline_unicode_all_rotations_crops_pixels_reopen_and_print_isolation() {
        let _print_lock = print_test_lock(); let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for original in [0, 90, 180, 270] { for edited in 0..4 {
            let bytes = crate::text_geometry::tests::fixture(original, [1.0, 0.0, 0.0, 1.0, 100.0, 372.0], "HiddenAbove\n\nFirst café\nSecond line\n\n\n\n\n\nHiddenBelow"); let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
            pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]); let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
            for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] { let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value); }
            let path = folder.path().join(format!("text-highlight-{original}-{edited}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); }
            let before = comments_png(&service, info.id, 0, 600); let text = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap();
            let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap(); assert_eq!(geometry.status, "ok"); let logical = geometry.characters.iter().map(|value| value.text.as_str()).collect::<String>(); assert!(logical.contains("First café") && logical.contains("Second line")); assert!(!logical.contains("HiddenAbove") && !logical.contains("HiddenBelow"));
            let start = geometry.characters.iter().position(|value| value.bounds.is_some()).unwrap(); let end = geometry.characters.iter().rposition(|value| value.bounds.is_some()).unwrap() + 1;
            let selected = &geometry.characters[start..end]; assert!(selected.iter().any(|value| value.bounds.is_none())); let expected = selected.iter().filter_map(|value| value.bounds.as_ref()).collect::<Vec<_>>();
            let ink = before.enumerate_pixels().filter(|(_, _, pixel)| pixel.0.iter().all(|value| *value < 40)).map(|(x, y, _)| (x as f64, y as f64)).collect::<Vec<_>>(); assert!(!ink.is_empty());
            assert!(ink.iter().all(|(x, y)| expected.iter().any(|rect| *x >= rect.x as f64 * before.width() as f64 - 2.0 && *x <= (rect.x + rect.width) as f64 * before.width() as f64 + 2.0 && *y >= rect.y as f64 * before.height() as f64 - 2.0 && *y <= (rect.y + rect.height) as f64 * before.height() as f64 + 2.0)), "Independent baseline ink must be inside selected mapped glyphs at {original}+{edited}");
            let old = print_snapshot(&service, info.id, info.revision); let old_pixels = service.print_render_blocking(old.token, 0, 600, 600).unwrap(); let body = "  Text description é 漢字 😀\n ";
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateTextHighlight(0, start, end, Some(body.into())), reply)).unwrap();
            let list = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); let annotation = &list.annotations[0]; let id = annotation.id.clone(); let quads = annotation.quads.as_ref().unwrap(); assert_eq!(quads.len(), expected.len()); assert_eq!(annotation.contents.as_deref(), Some(body));
            for (actual, expected) in quads.iter().zip(expected) { for (actual, expected) in [(actual.x, expected.x as f64), (actual.y, expected.y as f64), (actual.width, expected.width as f64), (actual.height, expected.height as f64)] { assert!((actual - expected).abs() < 0.000003); } }
            let after = comments_png(&service, info.id, 0, 600); text_highlights_assert_pixels(&before, &after, quads, 40); assert_eq!(after, comments_png(&service, info.id, 0, 600));
            assert_eq!(call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap(), text); assert_eq!(serde_json::to_value(call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap().characters).unwrap(), serde_json::to_value(&geometry.characters).unwrap());
            let snapshot = print_snapshot(&service, info.id, info.revision); let printed = service.print_render_blocking(snapshot.token, 0, 600, 600).unwrap(); text_highlights_assert_pixels(&comments_print_rgb(&old_pixels), &comments_print_rgb(&printed), quads, 40); assert_eq!(service.print_render_blocking(old.token, 0, 600, 600).unwrap().bgra, old_pixels.bgra);
            let copy = folder.path().join(format!("text-highlight-copy-{original}-{edited}.pdf")); info = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap().document; let reopened = call(&service, |reply| Request::Open(copy.clone(), reply)).unwrap(); assert_eq!(comments_png(&service, reopened.id, 0, 600), after);
            assert_eq!(serde_json::to_value(call(&service, |reply| Request::Annotations(reopened.id, 0, reply)).unwrap().annotations).unwrap(), serde_json::to_value(&list.annotations).unwrap()); let saved = lopdf::Document::load(&copy).unwrap(); assert_eq!(crate::comments::read(&saved).unwrap()[0][0].id, id);
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::DeleteHighlight(id), reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 600), before); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 600), after); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 600), before); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 600), after);
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.99, y: 0.0, width: 0.01, height: 1.0 }, reply)).unwrap(); let hidden = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert!(hidden.annotations[0].rect.is_none() && hidden.annotations[0].quads.as_ref().unwrap().is_empty());
            assert!(!comments_png(&service, info.id, 0, 6).pixels().any(|pixel| pixel[0] > 240 && pixel[1] > 240 && (180..205).contains(&pixel[2])));
            for id in [info.id, reopened.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); } assert_eq!(service.print_render_blocking(snapshot.token, 0, 600, 600).unwrap().bgra, printed.bgra); assert_eq!(std::fs::read(path).unwrap(), source);
        } }
    }
    #[test]
    fn text_highlights_overlapping_glyphs_use_one_fill_at_all_cardinal_glyph_angles() {
        use lopdf::{content::{Content, Operation}, Object};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for (angle, matrix) in [(0, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0]), (90, [0.0, 1.0, -1.0, 0.0, 250.0, 100.0]), (180, [-1.0, 0.0, 0.0, -1.0, 250.0, 250.0]), (270, [0.0, -1.0, 1.0, 0.0, 100.0, 250.0])] {
            let mut pdf = lopdf::Document::load_mem(&crate::text_geometry::tests::fixture(90, matrix, "MMM")).unwrap(); let page = pdf.get_pages()[&1]; let content = pdf.get_dictionary(page).unwrap().get(b"Contents").unwrap().as_reference().unwrap();
            let mut operations = Content::decode(&pdf.get_page_content(page)).unwrap().operations; operations.insert(2, Operation::new("Tc", vec![Object::Real(-12.0)])); pdf.get_object_mut(content).unwrap().as_stream_mut().unwrap().set_plain_content(Content { operations }.encode().unwrap());
            let path = folder.path().join(format!("overlapping-glyphs-{angle}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
            let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap(); assert_eq!(geometry.status, "ok", "{:?}", geometry.reason); assert_eq!(geometry.characters.len(), 3); assert!(geometry.characters.iter().all(|value| value.text == "M"));
            let before = comments_png(&service, info.id, 0, 600); info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateTextHighlight(0, 0, 3, None), reply)).unwrap(); let list = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); let quads = list.annotations[0].quads.as_ref().unwrap();
            let (a, b) = (&quads[0], &quads[1]); let overlap = (a.x.max(b.x), a.y.max(b.y), (a.x + a.width).min(b.x + b.width), (a.y + a.height).min(b.y + b.height)); assert!(overlap.2 > overlap.0 + 0.01 && overlap.3 > overlap.1 + 0.01, "Fixture must independently create substantial overlapping glyph bounds");
            let after = comments_png(&service, info.id, 0, 600); text_highlights_assert_pixels(&before, &after, quads, 40);
            assert!(after.enumerate_pixels().any(|(x, y, pixel)| x as f64 > overlap.0 * after.width() as f64 + 2.0 && (x as f64) < overlap.2 * after.width() as f64 - 2.0 && y as f64 > overlap.1 * after.height() as f64 + 2.0 && (y as f64) < overlap.3 * after.height() as f64 - 2.0 && before.get_pixel(x, y).0.iter().all(|value| *value > 240) && (pixel[2] as i16 - 191).abs() <= 2), "Overlap must contain white background tinted exactly once, not just solid black ink");
            let copy = folder.path().join(format!("overlap-copy-{angle}.pdf")); call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap(); let reopened = call(&service, |reply| Request::Open(copy, reply)).unwrap(); assert_eq!(comments_png(&service, reopened.id, 0, 600), after);
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::DeleteHighlight(list.annotations[0].id.clone()), reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 600), before); for id in [info.id, reopened.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); } assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn text_highlights_astral_mapping_is_whole_or_explicitly_unsupported() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let mut pdf = lopdf::Document::load_mem(&crate::text_geometry::tests::fixture(0, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "A")).unwrap(); let page = pdf.get_pages()[&1];
        let font = pdf.get_dictionary(page).unwrap().get(b"Resources").unwrap().as_dict().unwrap().get(b"Font").unwrap().as_dict().unwrap().get(b"F1").unwrap().as_reference().unwrap();
        let cmap = pdf.add_object(lopdf::Stream::new(lopdf::dictionary! {}, b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /Test def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfchar <41> <D83DDE00> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end".to_vec())); pdf.get_dictionary_mut(font).unwrap().set("ToUnicode", cmap);
        let path = folder.path().join("astral-mapping.pdf"); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 600); let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, 0, reply)).unwrap();
        if geometry.status == "ok" { assert_eq!(geometry.characters.len(), 1); assert_eq!(geometry.characters[0].text, "😀"); assert_eq!(geometry.characters[0].text.encode_utf16().count(), 2); info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).unwrap(); let annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); text_highlights_assert_pixels(&before, &comments_png(&service, info.id, 0, 600), annotations.annotations[0].quads.as_ref().unwrap(), 40); eprintln!("Astral PDFium mapping: supported as one geometry character"); }
        else { assert_eq!(geometry.status, "unsupported"); assert!(geometry.characters.is_empty() && geometry.reason.as_ref().unwrap().contains("character mapping")); assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).is_err()); assert_eq!(comments_png(&service, info.id, 0, 600), before); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty()); eprintln!("Astral PDFium mapping: explicitly unsupported; no partial geometry or mutation"); }
        call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert_eq!(std::fs::read(path).unwrap(), source);
    }
    #[test]
    fn text_highlights_mixed_corpus_structural_copies_reopen_and_source_preservation() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 1200); let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, 0, reply)).unwrap(); assert_eq!(geometry.status, "ok");
            let pattern = "Sample document".chars().map(|value| value.to_string()).collect::<Vec<_>>(); let start = geometry.characters.windows(pattern.len()).position(|window| window.iter().zip(&pattern).all(|(a, b)| &a.text == b)).expect("Generated corpus has an independently known embedded footer, including the scan; this is not OCR"); let end = start + pattern.len();
            info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, start, end, Some("  Footer description é 漢字 😀\n ".into())), reply)).unwrap(); let annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); let text_id = annotations.annotations[0].id.clone(); text_highlights_assert_pixels(&before, &comments_png(&service, info.id, 0, 1200), annotations.annotations[0].quads.as_ref().unwrap(), 160);
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, CropRect { x: 0.02, y: 0.02, width: 0.05, height: 0.05 }, None), reply)).unwrap();
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create((count - 1) as u16, CropRect { x: 0.02, y: 0.02, width: 0.05, height: 0.05 }, "Legacy-compatible note é 😀".into()), reply)).unwrap();
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.0, y: 0.0, width: 0.95, height: 0.99 }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: count - 1 }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1] }, reply)).unwrap();
            let expected = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert_eq!(expected.annotations.len(), 3); assert_eq!(expected.annotations.iter().find(|value| value.id == text_id).unwrap().page, count - 2); let pixels = comments_png(&service, info.id, (count - 2) as u16, 600);
            let copy = folder.path().join(format!("text-mixed-corpus-{count}.pdf")); info = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap().document; assert!(info.dirty); let reopened = call(&service, |reply| Request::Open(copy, reply)).unwrap(); assert_eq!(reopened.pages.len(), count - 1); assert_eq!(serde_json::to_value(call(&service, |reply| Request::Annotations(reopened.id, 0, reply)).unwrap().annotations).unwrap(), serde_json::to_value(&expected.annotations).unwrap()); assert_eq!(comments_png(&service, reopened.id, (count - 2) as u16, 600), pixels);
            let extracted = folder.path().join(format!("text-extracted-{count}.pdf")); call(&service, |reply| Request::Save(info.id, Some(vec![0, count - 2]), extracted.clone(), reply)).unwrap(); let extracted = call(&service, |reply| Request::Open(extracted, reply)).unwrap(); let annotations = call(&service, |reply| Request::Annotations(extracted.id, 0, reply)).unwrap(); assert_eq!(annotations.annotations.len(), 2); assert!(annotations.annotations.iter().any(|value| value.id == text_id && value.page == 1)); assert_eq!(comments_png(&service, extracted.id, 1, 600), pixels);
            let split = call(&service, |reply| Request::Split(info.id, info.revision, count.div_ceil(3), folder.path().join(format!("text-split-{count}")), reply)).unwrap(); let mut ids = Vec::new(); for file in split.files { let part = call(&service, |reply| Request::Open(file.path, reply)).unwrap(); ids.extend(call(&service, |reply| Request::Annotations(part.id, 0, reply)).unwrap().annotations.into_iter().map(|value| value.id)); call(&service, |reply| Request::Close(part.id, reply)).unwrap(); } assert_eq!(ids.len(), 3); assert!(ids.contains(&text_id)); assert!(call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap().dirty);
            let donor = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); for result in [call(&service, |reply| Request::CheckCombine(combine_source(&info), combine_source(&donor), reply)), call(&service, |reply| Request::CheckInsertion(combine_source(&info), combine_source(&donor), 0, reply)), call(&service, |reply| Request::CheckReplacement(combine_source(&info), combine_source(&donor), 0, 1, reply))] { assert!(result.unwrap_err().contains("Annots")); }
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![count - 2] }, reply)).unwrap(); assert!(!call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.iter().any(|value| value.id == text_id)); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, (count - 2) as u16, 600), pixels);
            for id in [info.id, reopened.id, extracted.id, donor.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); } assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn text_highlights_caps_invalid_stale_closed_preclosed_unsupported_and_truncation_are_atomic() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let mut pdf = lopdf::Document::load_mem(&crate::text_geometry::tests::fixture(90, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], &"M".repeat(257))).unwrap(); let page = pdf.get_pages()[&1]; for key in ["MediaBox", "CropBox"] { pdf.get_dictionary_mut(page).unwrap().set(key, vec![0.into(), 0.into(), 10_000.into(), 400.into()]); }
        let path = folder.path().join("quad-cap.pdf"); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 200); let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, 0, reply)).unwrap(); assert_eq!(geometry.characters.len(), 257);
        for (page, start, end, contents) in [(0, 0, 257, None), (1, 0, 1, None), (0, 2, 1, None), (0, 0, 0, None), (0, 0, 258, None), (0, usize::MAX, usize::MAX, None), (0, 0, 1, Some("\0".into())), (0, 0, 1, Some("x".repeat(8193)))] {
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(page, start, end, contents), reply)).is_err()); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty()); assert_eq!(comments_png(&service, info.id, 0, 200), before);
        }
        let (reply, canceled) = oneshot::channel(); drop(canceled); service.sender.send(Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).unwrap(); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty());
        info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 256, None), reply)).unwrap(); assert_eq!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations[0].quads.as_ref().unwrap().len(), 256);
        for _ in 0..15 { info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateTextHighlight(0, 0, 256, None), reply)).unwrap(); }
        let complete = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert_eq!(complete.annotations.iter().map(|value| value.quads.as_ref().unwrap().len()).sum::<usize>(), 4096, "The accepted query must never silently truncate geometry"); let accepted = comments_png(&service, info.id, 0, 200);
        assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).err().unwrap().contains("4,096")); assert_eq!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.len(), 16); assert_eq!(comments_png(&service, info.id, 0, 200), accepted);
        assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).is_err()); assert_eq!(comments_png(&service, info.id, 0, 200), accepted); call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).is_err()); assert_eq!(std::fs::read(path).unwrap(), source);
        for (label, matrix, text) in [("skewed", [1.0, 0.0, 0.2, 1.0, 100.0, 200.0], "MMMM".to_owned()), ("truncated", [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "M".repeat(20_001)), ("whitespace", [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "First café\nSecond line".to_owned())] {
            let rotation = if label == "whitespace" { 0 } else { 90 }; let path = folder.path().join(format!("range-{label}.pdf")); std::fs::write(&path, crate::text_geometry::tests::fixture(rotation, matrix, &text)).unwrap(); let source = std::fs::read(&path).unwrap(); let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 300); let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, 0, reply)).unwrap();
            let start = if label == "whitespace" { geometry.characters.iter().position(|value| value.bounds.is_none()).unwrap() } else { 0 }; assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, start, start + 1, None), reply)).is_err()); if label == "skewed" { assert_eq!(geometry.status, "unsupported"); } if label == "truncated" { assert!(geometry.truncated && geometry.characters.is_empty()); }
            assert_eq!(comments_png(&service, info.id, 0, 300), before); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty()); assert!(!call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap().can_undo); call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn highlights_all_rotations_inherited_crop_multiply_pixels_unicode_reopen_and_print_isolation() {
        let _print_lock = print_test_lock(); let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let marker = CropRect { x: 0.05, y: 0.05, width: 0.9, height: 0.9 };
        for original in [0, 90, 180, 270] { for edited in 0..4 {
            let bytes = crate::text_geometry::tests::fixture(original, [1.0, 0.0, 0.0, 1.0, 100.0, 250.0], "VisibleHeader\nVisibleBody"); let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
            pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]); let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
            for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] { let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value); }
            let path = folder.path().join(format!("highlight-{original}-{edited}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); }
            let before = comments_png(&service, info.id, 0, 400); let text = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap(); let geometry = serde_json::to_value(call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap().characters).unwrap();
            let old = print_snapshot(&service, info.id, info.revision); let old_pixels = service.print_render_blocking(old.token, 0, 400, 400).unwrap(); let dimensions = (info.pages[0].width, info.pages[0].height); let body = "  Area-only é 漢字 😀\n ";
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, marker, Some(body.into())), reply)).unwrap(); assert_eq!((info.pages[0].width, info.pages[0].height), dimensions);
            assert_eq!(call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap(), text); assert_eq!(serde_json::to_value(call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap().characters).unwrap(), geometry); assert!(call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes.is_empty());
            let annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert_eq!(annotations.annotations.len(), 1); let highlight = &annotations.annotations[0]; assert_eq!(highlight.kind, crate::comments::AnnotationKind::Highlight); assert_eq!(highlight.contents.as_deref(), Some(body)); let id = highlight.id.clone(); let displayed = highlight.rect.as_ref().unwrap();
            for (actual, expected) in [(displayed.x, marker.x), (displayed.y, marker.y), (displayed.width, marker.width), (displayed.height, marker.height)] { assert!((actual - expected).abs() < 0.00001); }
            let after = comments_png(&service, info.id, 0, 400); highlights_assert_pixels(&before, &after, marker, 40); highlights_assert_extent(&after, marker); assert_eq!(comments_png(&service, info.id, 0, 400), after); assert_eq!(service.print_render_blocking(old.token, 0, 400, 400).unwrap().bgra, old_pixels.bgra);
            let snapshot = print_snapshot(&service, info.id, info.revision); let snapshot_pixels = service.print_render_blocking(snapshot.token, 0, 400, 400).unwrap(); highlights_assert_pixels(&comments_print_rgb(&old_pixels), &comments_print_rgb(&snapshot_pixels), marker, 40);
            let copy = folder.path().join(format!("highlight-copy-{original}-{edited}.pdf")); info = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap().document; let reopened = call(&service, |reply| Request::Open(copy, reply)).unwrap(); assert_eq!(comments_png(&service, reopened.id, 0, 400), after); let imported = call(&service, |reply| Request::Annotations(reopened.id, 0, reply)).unwrap(); assert_eq!(imported.annotations[0].id, id); assert_eq!(imported.annotations[0].contents.as_deref(), Some(body)); assert_eq!(call(&service, |reply| Request::Text(reopened.id, 0, 0, reply)).unwrap(), text); call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.2, y: 0.0, width: 0.8, height: 1.0 }, reply)).unwrap(); let clipped = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); let clipped = clipped.annotations[0].rect.as_ref().unwrap(); assert!(clipped.x.abs() < 0.00001 && (clipped.width - 0.9375).abs() < 0.00001 && (clipped.y - 0.05).abs() < 0.00001);
            highlights_assert_extent(&comments_png(&service, info.id, 0, 320), CropRect { x: 0.0, y: 0.05, width: 0.9375, height: 0.9 });
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.99, y: 0.0, width: 0.01, height: 1.0 }, reply)).unwrap(); let hidden = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert!(hidden.annotations[0].rect.is_none()); assert_eq!(hidden.annotations[0].contents.as_deref(), Some(body));
            assert!(!comments_png(&service, info.id, 0, 2).pixels().any(|pixel| pixel[0] > 240 && pixel[1] > 240 && (180..205).contains(&pixel[2])), "Off-crop markup must not remain visible");
            let hidden_path = folder.path().join(format!("highlight-hidden-{original}-{edited}.pdf")); call(&service, |reply| Request::Save(info.id, None, hidden_path.clone(), reply)).unwrap(); let hidden_open = call(&service, |reply| Request::Open(hidden_path, reply)).unwrap(); assert!(call(&service, |reply| Request::Annotations(hidden_open.id, 0, reply)).unwrap().annotations[0].rect.is_none()); call(&service, |reply| Request::Close(hidden_open.id, reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 400), after); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 400), before); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 400), after); info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::DeleteHighlight(id), reply)).unwrap(); assert_eq!(comments_png(&service, info.id, 0, 400), before);
            call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert_eq!(service.print_render_blocking(snapshot.token, 0, 400, 400).unwrap().bgra, snapshot_pixels.bgra); assert_eq!(std::fs::read(path).unwrap(), source);
        } }
    }
    #[test]
    fn highlights_mixed_corpus_reopen_move_delete_extract_split_and_source_preservation() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap(); let area = CropRect { x: 0.15, y: 0.15, width: 0.7, height: 0.7 };
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 400);
            info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateHighlight(0, area, None), reply)).unwrap(); highlights_assert_pixels(&before, &comments_png(&service, info.id, 0, 400), area, 160);
            let first = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert!(first.annotations[0].contents.is_none()); let highlight = first.annotations[0].id.clone();
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create((count - 1) as u16, CropRect { x: 0.03, y: 0.03, width: 0.06, height: 0.06 }, "Retained legacy-compatible note é 😀".into()), reply)).unwrap();
            let notes = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap(); assert_eq!(notes.notes.len(), 1); let note = notes.notes[0].id.clone(); assert_ne!(note, highlight);
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::UpdateHighlight(highlight.clone(), Some("  Corpus highlight 漢字 😀\n ".into())), reply)).unwrap();
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.0, y: 0.0, width: 0.9, height: 0.9 }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: count - 1 }, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1] }, reply)).unwrap();
            let expected = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); assert_eq!(expected.annotations.len(), 2); assert_eq!(expected.annotations.iter().find(|value| value.id == highlight).unwrap().page, count - 2); assert_eq!(expected.annotations.iter().find(|value| value.id == note).unwrap().page, count - 3);
            let copy = folder.path().join(format!("mixed-corpus-{count}.pdf")); info = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap().document; assert!(info.dirty); let reopened = call(&service, |reply| Request::Open(copy, reply)).unwrap(); assert_eq!(reopened.pages.len(), count - 1); assert_eq!(serde_json::to_value(call(&service, |reply| Request::Annotations(reopened.id, 0, reply)).unwrap().annotations).unwrap(), serde_json::to_value(&expected.annotations).unwrap());
            for page in [count - 3, count - 2] { assert_eq!(comments_png(&service, info.id, page as u16, 400), comments_png(&service, reopened.id, page as u16, 400)); }
            let extracted = folder.path().join(format!("highlight-extracted-{count}.pdf")); call(&service, |reply| Request::Save(info.id, Some(vec![0, count - 2]), extracted.clone(), reply)).unwrap(); let extracted = call(&service, |reply| Request::Open(extracted, reply)).unwrap(); let annotations = call(&service, |reply| Request::Annotations(extracted.id, 0, reply)).unwrap(); assert_eq!(annotations.annotations.len(), 1); assert_eq!(annotations.annotations[0].kind, crate::comments::AnnotationKind::Highlight); assert_eq!(annotations.annotations[0].id, highlight); assert_eq!(annotations.annotations[0].page, 1); assert_eq!(comments_png(&service, extracted.id, 1, 400), comments_png(&service, info.id, (count - 2) as u16, 400));
            let split = call(&service, |reply| Request::Split(info.id, info.revision, count.div_ceil(3), folder.path().join(format!("mixed-split-{count}")), reply)).unwrap(); let mut ids = Vec::new();
            for file in split.files { let part = call(&service, |reply| Request::Open(file.path.clone(), reply)).unwrap(); for annotation in call(&service, |reply| Request::Annotations(part.id, 0, reply)).unwrap().annotations { ids.push(annotation.id); } call(&service, |reply| Request::Close(part.id, reply)).unwrap(); }
            assert_eq!(ids.len(), 2); assert!(ids.contains(&highlight) && ids.contains(&note)); assert!(call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap().dirty, "Save a Copy must preserve the original dirty session");
            let donor = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); assert!(call(&service, |reply| Request::CheckCombine(combine_source(&info), combine_source(&donor), reply)).unwrap_err().contains("Annots")); assert!(call(&service, |reply| Request::CheckInsertion(combine_source(&info), combine_source(&donor), 0, reply)).unwrap_err().contains("Annots")); assert!(call(&service, |reply| Request::CheckReplacement(combine_source(&info), combine_source(&donor), 0, 1, reply)).unwrap_err().contains("Annots"));
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![count - 2] }, reply)).unwrap(); assert!(!call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.iter().any(|value| value.id == highlight)); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.iter().any(|value| value.id == highlight));
            for id in [info.id, reopened.id, extracted.id, donor.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); } assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn highlights_invalid_stale_closed_preclosed_noops_and_branching_are_guarded() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let path = root.join("resources/welcome.pdf"); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let before = comments_png(&service, info.id, 0, 300); let rect = CropRect { x: 0.1, y: 0.1, width: 0.2, height: 0.2 };
        for (page, bounds, body) in [(6, rect, None), (0, CropRect { x: f64::NAN, ..rect }, None), (0, CropRect { width: 0.00001, ..rect }, None), (0, CropRect { x: 0.9, ..rect }, None), (0, rect, Some("\0".into())), (0, rect, Some("x".repeat(8193)))] {
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateHighlight(page, bounds, body), reply)).is_err()); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty()); assert_eq!(comments_png(&service, info.id, 0, 300), before);
        }
        let (reply, canceled) = oneshot::channel(); drop(canceled); service.sender.send(Request::Comment(info.id, 0, CommentMutation::CreateHighlight(0, rect, None), reply)).unwrap(); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().annotations.is_empty()); assert!(!call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap().can_undo);
        info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateHighlight(0, rect, Some(" \n\t".into())), reply)).unwrap(); let list = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap(); let first = list.annotations[0].id.clone(); assert!(list.annotations[0].contents.is_none());
        assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::UpdateHighlight(first.clone(), Some("Stale".into())), reply)).is_err()); assert!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).is_err()); assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Delete(first.clone()), reply)).is_err());
        let unchanged = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::UpdateHighlight(first.clone(), None), reply)).unwrap(); assert_eq!(unchanged.revision, info.revision); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create(0, rect, "Branch note".into()), reply)).unwrap(); let note = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes[0].id.clone(); assert!(crate::comments::number(&note).unwrap() > crate::comments::number(&first).unwrap()); assert!(!info.can_redo); assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::DeleteHighlight(note), reply)).is_err());
        info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, rect, None), reply)).unwrap(); let highlight = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.into_iter().find(|value| value.kind == crate::comments::AnnotationKind::Highlight).unwrap().id; info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::DeleteHighlight(highlight), reply)).unwrap(); assert_eq!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations.len(), 1);
        call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).is_err()); assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, rect, None), reply)).is_err()); assert_eq!(std::fs::read(path).unwrap(), source);
    }
    #[test]
    fn comments_rotations_inherited_crop_pixels_unicode_reopen_and_print_snapshots_agree() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let marker = CropRect { x: 0.1, y: 0.1, width: 0.2, height: 0.2 };
        for original in [0, 90, 180, 270] { for edited in 0..4 {
            let bytes = crate::text_geometry::tests::fixture(original, [1.0, 0.0, 0.0, 1.0, 100.0, 250.0], "VisibleHeader\nVisibleBody");
            let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
            pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]);
            let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
            for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] { let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value); }
            let path = folder.path().join(format!("notes-{original}-{edited}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap();
            let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); }
            let baseline = comments_png(&service, info.id, 0, 600); assert!(comments_yellow_bounds(&baseline).is_none());
            let text = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap();
            let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap(); let chars = serde_json::to_value(&geometry.characters).unwrap();
            let old_snapshot = print_snapshot(&service, info.id, info.revision); let before_print = service.print_render_blocking(old_snapshot.token, 0, 600, 600).unwrap();
            let dimensions = (info.pages[0].width, info.pages[0].height); let note_body = "Note-only é 漢字 😀\nSecond line";
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create(0, marker, note_body.into()), reply)).unwrap();
            assert_eq!((info.pages[0].width, info.pages[0].height), dimensions);
            assert_eq!(call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap(), text);
            assert_eq!(serde_json::to_value(call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap().characters).unwrap(), chars);
            let list = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap(); assert_eq!(list.status, "supported"); assert_eq!(list.notes.len(), 1); assert_eq!(list.notes[0].contents, note_body);
            let note_id = list.notes[0].id.clone(); let rect = list.notes[0].rect.as_ref().unwrap();
            for (actual, expected) in [(rect.x, marker.x), (rect.y, marker.y), (rect.width, marker.width), (rect.height, marker.height)] { assert!((actual - expected).abs() < 0.00001); }
            let preview = comments_png(&service, info.id, 0, 600); comments_assert_box(&preview, marker); assert_eq!(comments_png(&service, info.id, 0, 600), preview);
            assert_eq!(service.print_render_blocking(old_snapshot.token, 0, 600, 600).unwrap().bgra, before_print.bgra, "An earlier snapshot must not acquire the new note");
            let snapshot = print_snapshot(&service, info.id, info.revision); comments_assert_box(&comments_print_rgb(&service.print_render_blocking(snapshot.token, 0, 600, 600).unwrap()), marker);
            let output_path = folder.path().join(format!("notes-copy-{original}-{edited}.pdf")); let saved = call(&service, |reply| Request::Save(info.id, None, output_path.clone(), reply)).unwrap(); info = saved.document;
            let output = lopdf::Document::load(&output_path).unwrap(); let output_page = output.get_pages()[&1]; assert_eq!(output.get_dictionary(output_page).unwrap().get(b"Annots").unwrap().as_array().unwrap().len(), 1);
            let reopened = call(&service, |reply| Request::Open(output_path, reply)).unwrap(); assert_eq!(call(&service, |reply| Request::Comments(reopened.id, 0, reply)).unwrap().notes[0].contents, note_body); comments_assert_box(&comments_png(&service, reopened.id, 0, 600), marker);
            assert_eq!(call(&service, |reply| Request::Text(reopened.id, 0, 0, reply)).unwrap(), text); call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.2, y: 0.0, width: 0.8, height: 1.0 }, reply)).unwrap();
            let clipped = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap(); let clipped = clipped.notes[0].rect.as_ref().unwrap(); assert!(clipped.x.abs() < 0.00001 && (clipped.width - 0.125).abs() < 0.00001);
            comments_assert_box(&comments_png(&service, info.id, 0, 480), CropRect { x: 0.0, y: 0.1, width: 0.125, height: 0.2 });
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.5, y: 0.0, width: 0.5, height: 1.0 }, reply)).unwrap();
            let hidden = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap(); assert!(hidden.notes[0].rect.is_none()); assert_eq!(hidden.notes[0].contents, note_body); assert!(comments_yellow_bounds(&comments_png(&service, info.id, 0, 240)).is_none());
            let hidden_path = folder.path().join(format!("hidden-{original}-{edited}.pdf")); let hidden_output = call(&service, |reply| Request::Save(info.id, None, hidden_path.clone(), reply)).unwrap(); assert_eq!(hidden_output.document.revision, info.revision);
            let hidden_reopen = call(&service, |reply| Request::Open(hidden_path, reply)).unwrap(); let hidden = call(&service, |reply| Request::Comments(hidden_reopen.id, 0, reply)).unwrap(); assert!(hidden.notes[0].rect.is_none()); assert_eq!(hidden.notes[0].id, note_id); call(&service, |reply| Request::Close(hidden_reopen.id, reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); comments_assert_box(&comments_png(&service, info.id, 0, 600), marker);
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Delete(note_id), reply)).unwrap(); assert!(call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes.is_empty()); assert_eq!(comments_png(&service, info.id, 0, 600), baseline);
            call(&service, |reply| Request::Close(info.id, reply)).unwrap(); comments_assert_box(&comments_print_rgb(&service.print_render_blocking(snapshot.token, 0, 600, 600).unwrap()), marker);
            assert_eq!(std::fs::read(path).unwrap(), source);
        } }
    }
    #[test]
    fn comments_corpus_copy_reopen_move_delete_extract_and_split_preserve_notes_and_sources() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap(); let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            let marker = CropRect { x: 0.04, y: 0.04, width: 0.08, height: 0.08 };
            for page in [0, count - 1] {
                let before = comments_png(&service, info.id, page as u16, 400);
                info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create(page as u16, marker, format!("Note on source page {page} é 😀")), reply)).unwrap();
                let after = comments_png(&service, info.id, page as u16, 400); assert_ne!(after, before, "A corpus marker must appear in actual rendered pixels");
                for (x, y, pixel) in after.enumerate_pixels() {
                    let outside = (x as f64) < marker.x * after.width() as f64 - 2.0 || (x as f64) > (marker.x + marker.width) * after.width() as f64 + 2.0 || (y as f64) < marker.y * after.height() as f64 - 2.0 || (y as f64) > (marker.y + marker.height) * after.height() as f64 + 2.0;
                    if outside { assert_eq!(*pixel, *before.get_pixel(x, y), "Notes must not change unrelated corpus pixels"); }
                }
            }
            let first_id = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes[0].id.clone();
            info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Update(first_id.clone(), "Edited Unicode 漢字 😀".into()), reply)).unwrap();
            info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.0, y: 0.0, width: 0.9, height: 0.9 }, reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: count - 1 }, reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![1] }, reply)).unwrap();
            let list = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap(); assert_eq!(list.notes.len(), 2); assert_eq!(list.notes.iter().find(|note| note.id == first_id).unwrap().page, count - 2);
            let output_path = folder.path().join(format!("corpus-notes-{count}.pdf")); let saved = call(&service, |reply| Request::Save(info.id, None, output_path.clone(), reply)).unwrap(); info = saved.document; assert!(info.dirty);
            let reopened = call(&service, |reply| Request::Open(output_path, reply)).unwrap(); assert_eq!(reopened.pages.len(), count - 1);
            let reopened_list = call(&service, |reply| Request::Comments(reopened.id, 0, reply)).unwrap(); assert_eq!(serde_json::to_value(list.notes).unwrap(), serde_json::to_value(reopened_list.notes).unwrap());
            for page in [count - 3, count - 2] { assert_eq!(comments_png(&service, info.id, page as u16, 400), comments_png(&service, reopened.id, page as u16, 400)); }
            let extraction = folder.path().join(format!("extracted-notes-{count}.pdf")); call(&service, |reply| Request::Save(info.id, Some(vec![0, count - 2]), extraction.clone(), reply)).unwrap();
            let extracted = call(&service, |reply| Request::Open(extraction, reply)).unwrap(); let notes = call(&service, |reply| Request::Comments(extracted.id, 0, reply)).unwrap(); assert_eq!(notes.notes.len(), 1); assert_eq!(notes.notes[0].id, first_id); assert_eq!(notes.notes[0].page, 1); assert_eq!(notes.notes[0].contents, "Edited Unicode 漢字 😀");
            if count == 6 {
                let split = call(&service, |reply| Request::Split(info.id, info.revision, 2, folder.path().join("notes-split"), reply)).unwrap(); let mut ids = Vec::new();
                for file in split.files { let part = call(&service, |reply| Request::Open(file.path.clone(), reply)).unwrap(); ids.extend(call(&service, |reply| Request::Comments(part.id, 0, reply)).unwrap().notes.into_iter().map(|note| note.id)); call(&service, |reply| Request::Close(part.id, reply)).unwrap(); }
                assert_eq!(ids.len(), 2); assert!(ids.contains(&first_id));
            }
            let donor = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
            assert!(call(&service, |reply| Request::CheckCombine(combine_source(&info), combine_source(&donor), reply)).unwrap_err().contains("Annots"));
            assert!(call(&service, |reply| Request::CheckInsertion(combine_source(&info), combine_source(&donor), 0, reply)).unwrap_err().contains("Annots"));
            assert!(call(&service, |reply| Request::CheckReplacement(combine_source(&info), combine_source(&donor), 0, 1, reply)).unwrap_err().contains("Annots"));
            let deleted_page = count - 2; info = call(&service, |reply| Request::Edit(info.id, PageEdit::Delete { pages: vec![deleted_page] }, reply)).unwrap(); assert!(!call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes.iter().any(|note| note.id == first_id));
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert!(call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes.iter().any(|note| note.id == first_id));
            for id in [info.id, reopened.id, extracted.id, donor.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
            assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    #[test]
    fn comments_invalid_stale_closed_preclosed_and_branch_ids_preserve_session_state() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let mut info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let marker = CropRect { x: 0.1, y: 0.1, width: 0.05, height: 0.05 }; let baseline = comments_png(&service, info.id, 0, 400);
        for (page, rect, contents) in [(99, marker, "text".into()), (0, CropRect { x: f64::NAN, ..marker }, "text".into()), (0, CropRect { x: -0.1, ..marker }, "text".into()), (0, CropRect { width: 0.000001, ..marker }, "text".into()), (0, marker, " \n".into()), (0, marker, "x\0y".into()), (0, marker, "x".repeat(8193))] {
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::Create(page, rect, contents), reply)).is_err());
            assert!(call(&service, |reply| Request::Comments(info.id, 0, reply)).unwrap().notes.is_empty());
        }
        let (reply, unread) = oneshot::channel(); drop(unread); service.sender.send(Request::Comment(info.id, 0, CommentMutation::Create(0, marker, "Canceled".into()), reply)).unwrap();
        assert!(call(&service, |reply| Request::Comments(info.id, 0, reply)).unwrap().notes.is_empty()); assert_eq!(comments_png(&service, info.id, 0, 400), baseline);
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap(); assert_eq!(info.revision, 0); assert!(!info.dirty && !info.can_undo);
        info = call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::Create(0, marker, "First".into()), reply)).unwrap(); let id = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes[0].id.clone();
        assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::Update(id.clone(), "Stale".into()), reply)).err().unwrap().contains("changed")); assert!(call(&service, |reply| Request::Comments(info.id, 0, reply)).is_err());
        let noop = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Update(id.clone(), "First".into()), reply)).unwrap(); assert_eq!(noop.revision, info.revision);
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert!(call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes.is_empty());
        info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create(0, marker, "Branch".into()), reply)).unwrap(); let next_id = call(&service, |reply| Request::Comments(info.id, info.revision, reply)).unwrap().notes[0].id.clone(); assert_ne!(next_id, id); assert!(!info.can_redo);
        assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Delete(id), reply)).is_err());
        call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert!(call(&service, |reply| Request::Comments(info.id, info.revision, reply)).is_err()); assert!(call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Delete(next_id), reply)).is_err());
    }
    #[test]
    fn comments_signed_forms_tagging_foreign_annotations_and_altered_owned_schema_refuse() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for feature in ["Perms", "AcroForm", "StructTreeRoot", "Foreign", "Altered"] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            match feature {
                "Foreign" => { let page = pdf.get_pages()[&1]; let annotation = pdf.add_object(lopdf::dictionary! { "Type" => "Annot", "Subtype" => "Text", "Rect" => vec![40.into(), 60.into(), 70.into(), 90.into()] }); pdf.get_dictionary_mut(page).unwrap().set("Annots", vec![lopdf::Object::Reference(annotation)]); },
                "Altered" => { let mut source = Vec::new(); pdf.save_to(&mut source).unwrap(); let mut session = EditSession::new(source, 6); let plan = session.proposed_comment(Some((0, CropBox { left: 40.0, bottom: 60.0, right: 70.0, top: 90.0 })), None, Some("Existing owned note")).unwrap(); session.commit_comments(plan); pdf = lopdf::Document::load_mem(&session.export(None).unwrap()).unwrap(); let page = pdf.get_pages()[&1]; let annotation = pdf.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap(); pdf.get_dictionary_mut(annotation).unwrap().set("Popup", lopdf::Object::Null); },
                feature => { pdf.catalog_mut().unwrap().set(feature, lopdf::dictionary! {}); },
            }
            let path = folder.path().join(format!("protected-notes-{feature}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap(); let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            let list = call(&service, |reply| Request::Comments(info.id, 0, reply)).unwrap(); assert_eq!(list.status, "unsupported"); assert!(list.reason.is_some()); assert!(list.notes.is_empty());
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::Create(0, CropRect { x: 0.1, y: 0.1, width: 0.1, height: 0.1 }, "Blocked".into()), reply)).is_err());
            assert_eq!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().status, "unsupported"); assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateHighlight(0, CropRect { x: 0.1, y: 0.1, width: 0.1, height: 0.1 }, None), reply)).is_err());
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).is_err());
            call(&service, |reply| Request::Close(info.id, reply)).unwrap(); assert_eq!(std::fs::read(path).unwrap(), source);
        }
    }
    fn scheduler_render(id: u64) -> Request { let (reply, _) = oneshot::channel(); Request::Render(id, 0, 64, reply) }
    fn scheduler_cleanup(id: u64, print: bool) -> Request {
        let (reply, _) = oneshot::channel();
        if print { Request::EndPrint(id, reply) } else { Request::Close(id, reply) }
    }
    #[test]
    fn worker_scheduler_preserves_every_nonrender_barrier_and_cleanup_order() {
        let first = CombineSource { id: 1, revision: 0 }; let second = CombineSource { id: 2, revision: 0 };
        let (tx, _) = oneshot::channel();
        let (begin_tx, _) = oneshot::channel(); let (print_tx, _) = oneshot::channel();
        let (save_tx, _) = oneshot::channel(); let (combine_tx, _) = oneshot::channel();
        let (text_tx, _) = oneshot::channel(); let (open_tx, _) = oneshot::channel();
        let (crop_tx, _) = oneshot::channel(); let (split_tx, _) = oneshot::channel();
        for barrier in [Request::Edit(1, PageEdit::Undo, tx), Request::BeginPrint(1, 0, begin_tx),
            Request::PrintRender(1, 0, 64, 64, print_tx), Request::Save(1, None, PathBuf::new(), save_tx),
            Request::Combine(first, second, PathBuf::new(), combine_tx), Request::Text(1, 0, 0, text_tx),
            Request::Open(PathBuf::new(), open_tx), Request::Crop(1, 0, 0, CropRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }, crop_tx),
            Request::Split(1, 0, 1, PathBuf::new(), split_tx)] {
            let (sender, receiver) = mpsc::channel();
            sender.send(scheduler_render(10)).unwrap(); sender.send(barrier).unwrap();
            sender.send(scheduler_cleanup(20, false)).unwrap(); sender.send(scheduler_cleanup(21, true)).unwrap();
            let mut queue = RequestQueue::default();
            assert!(matches!(queue.next(&receiver).unwrap(), Request::Render(10, ..)));
            assert!(!matches!(queue.next(&receiver).unwrap(), Request::Render(..) | Request::Close(..) | Request::EndPrint(..)));
            assert!(matches!(queue.next(&receiver).unwrap(), Request::Close(20, ..)));
            assert!(matches!(queue.next(&receiver).unwrap(), Request::EndPrint(21, ..)));
        }
    }
    #[test]
    fn worker_scheduler_window_is_bounded_and_render_stream_cannot_starve() {
        let (sender, receiver) = mpsc::channel();
        for id in 0..32 { sender.send(scheduler_render(id)).unwrap(); }
        sender.send(scheduler_cleanup(99, false)).unwrap();
        let mut queue = RequestQueue::default();
        assert!(matches!(queue.next(&receiver).unwrap(), Request::Render(0, ..)));
        assert_eq!(queue.deferred.len(), 31);
        assert!(matches!(queue.next(&receiver).unwrap(), Request::Close(99, ..)));
        assert_eq!(queue.deferred.len(), 31);
        for id in 100..110 { sender.send(scheduler_cleanup(id, false)).unwrap(); }
        assert!(matches!(queue.next(&receiver).unwrap(), Request::Close(100, ..)));
        assert!(matches!(queue.next(&receiver).unwrap(), Request::Render(1, ..)), "Two cleanup overtakes must be followed by ordinary render progress");
        let (sender, receiver) = mpsc::channel();
        for id in 0..100 { sender.send(scheduler_render(id)).unwrap(); }
        drop(sender);
        let mut queue = RequestQueue::default();
        for expected in 0..100 {
            assert!(matches!(queue.next(&receiver).unwrap(), Request::Render(id, ..) if id == expected));
            assert!(queue.deferred.len() < 32);
        }
        assert!(queue.next(&receiver).is_err());
    }
    #[test]
    fn worker_scheduler_fifo_control_discriminates_the_cleanup_regression() {
        let (sender, receiver) = mpsc::channel();
        let mut fifo = VecDeque::new();
        for id in 0..8 { fifo.push_back(scheduler_render(id)); }
        fifo.push_back(scheduler_cleanup(99, false)); fifo.push_back(scheduler_cleanup(100, true));
        let before_close = fifo.iter().take_while(|request| !matches!(request, Request::Close(..))).filter(|request| matches!(request, Request::Render(..))).count();
        assert_eq!(before_close, 8, "FIFO control must reproduce eight render dispatches before Close");
        let mut queue = RequestQueue { deferred: fifo, cleanup_overtakes: 0, pending: Arc::default() };
        assert!(matches!(queue.next(&receiver).unwrap(), Request::Close(99, ..)));
        assert!(matches!(queue.next(&receiver).unwrap(), Request::EndPrint(100, ..)));
        drop(sender);
    }
    fn wait_probe_marker(receiver: &mut oneshot::Receiver<Result<(), String>>, deadline: std::time::Instant) -> Option<std::time::Instant> {
        loop {
            match receiver.try_recv() {
                Ok(result) => { result.unwrap(); return Some(std::time::Instant::now()); }
                Err(oneshot::error::TryRecvError::Closed) => panic!("The worker stopped before its backlog marker reply"),
                Err(oneshot::error::TryRecvError::Empty) => {
                    if std::time::Instant::now() >= deadline { return None; }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
        }
    }
    #[test]
    fn worker_cleanup_passes_queued_live_viewer_renders_without_rendering_closed_document() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let snapshot = print_snapshot(&service, info.id, info.revision);
        let _cleanup = UnreadPrintCleanup { service: service.clone(), document: info.id, tokens: vec![snapshot.token] };
        let (release, gate) = mpsc::channel();
        call(&service, |reply| Request::BacklogGate(gate, reply)).unwrap();
        let mut responses = Vec::new();
        let mut batch = Vec::new();
        for index in 0..8 {
            let (tx, rx) = oneshot::channel();
            responses.push(rx);
            batch.push(Request::Render(info.id, 0, 64 + index * 64, tx));
        }
        let (close_tx, close_rx) = oneshot::channel();
        batch.push(Request::Close(info.id, close_tx));
        let (end_tx, end_rx) = oneshot::channel();
        batch.push(Request::EndPrint(snapshot.token, end_tx));
        for request in batch { service.sender.send(request).unwrap(); }
        release.send(Vec::new()).unwrap();
        close_rx.blocking_recv().unwrap().unwrap();
        end_rx.blocking_recv().unwrap().unwrap();
        let results: Vec<_> = responses.into_iter().map(|rx| rx.blocking_recv().unwrap()).collect();
        let work = call(&service, |reply| Request::RenderWorkForDocument(info.id, reply)).unwrap();
        assert_eq!(work.dequeued, 8);
        assert_eq!(work.skipped_closed, 0, "Viewer replies remain live; cleanup must prevent obsolete engine work");
        assert_eq!(work.rendered, 0, "Queued live viewer renders delayed cleanup and rendered a document being closed");
        assert!(results.iter().all(|result| result.as_ref().err().is_some_and(|error| error.contains("closed"))));
        assert!(service.print_render_blocking(snapshot.token, 0, 64, 64).err().unwrap().contains("ended"));
        assert!(call(&service, |reply| Request::Properties(info.id, info.revision, reply)).err().unwrap().contains("closed"));
    }
    #[test]
    #[ignore = "Manual bounded backlog measurement; no timing regression assertion"]
    fn worker_backlog_probe_measures_close_release_and_live_reply_retention() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let path = root.join("../test-corpus/synthetic-scan-98.pdf");
        let source = std::fs::read(&path).unwrap(); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (mode, count, preclosed) in [("baseline", 0, false), ("live_unread", 8, false), ("preclosed_receivers", 8, true)] {
            let experiment_start = std::time::Instant::now(); let deadline = experiment_start + std::time::Duration::from_secs(30);
            let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            let snapshot = call(&service, |reply| Request::BeginPrint(info.id, info.revision, reply)).unwrap();
            let _cleanup = UnreadPrintCleanup { service: service.clone(), document: info.id, tokens: vec![snapshot.token] };
            let mut responses = Vec::new();
            for index in 0..count {
                let (tx, rx) = oneshot::channel();
                if preclosed { drop(rx); } else { responses.push(rx); }
                service.sender.send(Request::Render(info.id, 0, 2048 + index * 64, tx)).unwrap();
            }
            let close_enqueued = std::time::Instant::now(); let (close_tx, mut close_rx) = oneshot::channel();
            service.sender.send(Request::Close(info.id, close_tx)).unwrap();
            let release_enqueued = std::time::Instant::now(); let (release_tx, mut release_rx) = oneshot::channel();
            service.sender.send(Request::EndPrint(snapshot.token, release_tx)).unwrap();
            let closed = wait_probe_marker(&mut close_rx, deadline); let released = wait_probe_marker(&mut release_rx, deadline);
            if closed.is_none() || released.is_none() {
                drop(responses);
                println!("backlog mode={mode} source_bytes={} requests={count} soft_budget_exceeded=true; pending receivers dropped to skip remaining work", source.len());
                continue;
            }
            let mut retained_png_bytes = 0usize;
            let mut closed_errors = 0usize;
            for receiver in responses {
                match receiver.blocking_recv().unwrap() {
                    Ok(bytes) => retained_png_bytes += bytes.len(),
                    Err(error) => { assert!(error.contains("closed")); closed_errors += 1; }
                }
            }
            let work = call(&service, |reply| Request::RenderWorkForDocument(info.id, reply)).unwrap();
            println!("backlog mode={mode} source_bytes={} requests={count} widths=2048..2496 step=64 page=0 close_ms={:.3} release_ms={:.3} elapsed_ms={:.3} retained_png_bytes={retained_png_bytes} closed_errors={closed_errors} dequeued={} skipped_closed={} rendered={}", source.len(), closed.unwrap().duration_since(close_enqueued).as_secs_f64() * 1000.0, released.unwrap().duration_since(release_enqueued).as_secs_f64() * 1000.0, experiment_start.elapsed().as_secs_f64() * 1000.0, work.dequeued, work.skipped_closed, work.rendered);
            assert_eq!(work.dequeued, count as usize);
            assert_eq!(work.skipped_closed, if preclosed { count as usize } else { 0 });
            assert_eq!(work.rendered + closed_errors, if preclosed { 0 } else { count as usize });
            if mode != "live_unread" { assert_eq!(retained_png_bytes, 0); }
            assert!(service.print_render_blocking(snapshot.token, 0, 64, 64).err().unwrap().contains("ended"));
            assert!(call(&service, |reply| Request::Properties(info.id, info.revision, reply)).err().unwrap().contains("closed"));
        }
        assert_eq!(std::fs::read(path).unwrap(), source);
    }
    struct UnreadPrintCleanup { service: PdfService, document: u64, tokens: Vec<u64> }
    impl Drop for UnreadPrintCleanup {
        fn drop(&mut self) {
            for token in &self.tokens {
                let (tx, rx) = oneshot::channel();
                if self.service.sender.send(Request::EndPrint(*token, tx)).is_ok() { let _ = rx.blocking_recv(); }
            }
            let (tx, rx) = oneshot::channel();
            if self.service.sender.send(Request::Close(self.document, tx)).is_ok() { let _ = rx.blocking_recv(); }
        }
    }
    #[test]
    fn print_unread_successful_replies_release_snapshot_capacity_when_receivers_are_dropped() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let bootstrap = print_snapshot(&service, info.id, info.revision);
        let first_token = bootstrap.token + 1;
        drop(bootstrap);
        let mut cleanup = UnreadPrintCleanup { service: service.clone(), document: info.id, tokens: Vec::new() };
        for index in 0..4 {
            let (tx, rx) = oneshot::channel();
            service.sender.send(Request::BeginPrint(info.id, info.revision, tx)).unwrap();
            call(&service, |reply| Request::Properties(info.id, info.revision, reply)).unwrap();
            assert!(!rx.is_empty(), "The worker barrier must establish that the print reply was already sent");
            cleanup.tokens.push(first_token + index);
            drop(rx);
            call(&service, |reply| Request::Properties(info.id, info.revision, reply)).unwrap();
        }
        let available = call(&service, |reply| Request::BeginPrint(info.id, info.revision, reply));
        assert!(available.is_ok(), "Dropped unread replies retained print capacity: {}", available.as_ref().err().map(String::as_str).unwrap_or(""));
        let available = TestPrintSnapshot { service: service.clone(), info: available.unwrap() };
        assert!(!service.print_render_blocking(available.token, 0, 100, 100).unwrap().bgra.is_empty());
        drop(available);
    }
    #[test]
    fn open_canceled_before_worker_processing_never_retains_an_unreachable_document() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap(); let path = folder.path().join("canceled-open.pdf");
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); std::fs::write(&path, &source).unwrap();
        let (tx, rx) = oneshot::channel(); drop(rx);
        service.sender.send(Request::Open(path.clone(), tx)).unwrap();
        // The probe follows Open on the same FIFO and observes only this test's unique path.
        let retained = call(&service, |reply| Request::OpenDocumentsForPath(path.clone(), reply)).unwrap();
        for id in &retained {
            assert_eq!(call(&service, |reply| Request::Properties(*id, 0, reply)).unwrap().page_count, 6);
            call(&service, |reply| Request::Close(*id, reply)).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), source);
        assert!(retained.is_empty(), "A canceled Open retained {} unreachable document(s)", retained.len());
    }
    struct UnreadReplyCleanup { service: PdfService, paths: Vec<PathBuf> }
    impl Drop for UnreadReplyCleanup {
        fn drop(&mut self) {
            for path in &self.paths {
                let (tx, rx) = oneshot::channel();
                if self.service.sender.send(Request::OpenDocumentsForPath(path.clone(), tx)).is_ok() {
                    if let Ok(Ok(ids)) = rx.blocking_recv() {
                        for id in ids {
                            let (tx, rx) = oneshot::channel();
                            if self.service.sender.send(Request::Close(id, tx)).is_ok() { let _ = rx.blocking_recv(); }
                        }
                    }
                }
                let (tx, rx) = oneshot::channel();
                if self.service.sender.send(Request::PasswordRequestsForPath(path.clone(), tx)).is_ok() {
                    if let Ok(Ok(ids)) = rx.blocking_recv() {
                        for id in ids {
                            let (tx, rx) = oneshot::channel();
                            if self.service.sender.send(Request::CancelPassword(id, tx)).is_ok() { let _ = rx.blocking_recv(); }
                        }
                    }
                }
            }
        }
    }
    fn unread_document_reply<T>(service: &PdfService, path: &PathBuf, request: impl FnOnce(Reply<T>) -> Request) -> Vec<u64> {
        let (tx, rx) = oneshot::channel(); service.sender.send(request(tx)).unwrap();
        let registered = call(service, |reply| Request::OpenDocumentsForPath(path.clone(), reply)).unwrap();
        assert_eq!(registered.len(), 1, "The worker barrier must observe the successfully registered document");
        assert!(!rx.is_empty(), "The result must already be sent before its unread receiver is dropped");
        drop(rx);
        call(service, |reply| Request::OpenDocumentsForPath(path.clone(), reply)).unwrap()
    }
    fn password_source(root: &PathBuf) -> Vec<u8> {
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        pdf.trailer.set("ID", vec![lopdf::Object::string_literal("unread-password-fixture"), lopdf::Object::string_literal("unread-password-fixture")]);
        let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner password", user_password: "test password", key_length: 128, permissions: lopdf::Permissions::all() };
        let state = lopdf::EncryptionState::try_from(encryption).unwrap(); pdf.encrypt(&state).unwrap();
        let mut bytes = Vec::new(); pdf.save_to(&mut bytes).unwrap(); bytes
    }
    #[test]
    fn unread_document_open_and_begin_open_success_replies_release_registered_sessions() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap();
        let mut cleanup = UnreadReplyCleanup { service: service.clone(), paths: Vec::new() }; let mut leaks = Vec::new();
        for begin_open in [false, true] {
            let path = folder.path().join(if begin_open { "begin-open.pdf" } else { "open.pdf" }); std::fs::write(&path, &source).unwrap(); cleanup.paths.push(path.clone());
            let retained = if begin_open { unread_document_reply(&service, &path, |reply| Request::BeginOpen(path.clone(), reply)) }
                else { unread_document_reply(&service, &path, |reply| Request::Open(path.clone(), reply)) };
            leaks.push((if begin_open { "BeginOpen" } else { "Open" }, retained.len()));
            assert_eq!(std::fs::read(&path).unwrap(), source);
        }
        assert!(leaks.iter().all(|(_, count)| *count == 0), "Successfully sent but unread document replies retained sessions: {leaks:?}");
    }
    #[test]
    fn unread_document_unlock_success_reply_releases_registered_session() {
        let _password_lock = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("unlock.pdf"); let source = password_source(&root); std::fs::write(&path, &source).unwrap();
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![path.clone()] };
        let challenge = call(&service, |reply| Request::BeginOpen(path.clone(), reply)).unwrap();
        let request_id = match challenge { OpenResult::PasswordRequired { request_id, .. } => request_id, _ => panic!("Expected password challenge") };
        let retained = unread_document_reply(&service, &path, |reply| Request::Unlock(request_id, "test password".into(), reply));
        assert_eq!(std::fs::read(&path).unwrap(), source);
        assert!(retained.is_empty(), "Successfully sent but unread Unlock retained {} session(s)", retained.len());
    }
    #[test]
    fn unread_document_combine_success_reply_releases_session_and_preserves_published_pdf() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); let first_path = folder.path().join("first.pdf"); let second_path = folder.path().join("second.pdf"); let output = folder.path().join("combined.pdf");
        std::fs::write(&first_path, &source).unwrap(); std::fs::write(&second_path, &source).unwrap();
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![first_path.clone(), second_path.clone(), output.clone()] };
        let first = call(&service, |reply| Request::Open(first_path.clone(), reply)).unwrap(); let second = call(&service, |reply| Request::Open(second_path.clone(), reply)).unwrap();
        let retained = unread_document_reply(&service, &output, |reply| Request::Combine(combine_source(&first), combine_source(&second), output.clone(), reply));
        assert_eq!(lopdf::Document::load(&output).unwrap().get_pages().len(), 12);
        let reopened = call(&service, |reply| Request::Open(output.clone(), reply)).unwrap(); assert_eq!(reopened.pages.len(), 12);
        call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
        assert_eq!(std::fs::read(&first_path).unwrap(), source); assert_eq!(std::fs::read(&second_path).unwrap(), source);
        for source in [&first, &second] { assert_eq!(call(&service, |reply| Request::Properties(source.id, 0, reply)).unwrap().page_count, 6); }
        assert!(retained.is_empty(), "Successfully sent but unread Combine retained {} session(s)", retained.len());
    }
    #[test]
    fn image_pdf_output_is_clean_printable_exportable_and_preserves_open_sessions() {
        let _print_lock = print_test_lock(); let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source.data"); let output = folder.path().join("created.pdf"); let exported = folder.path().join("exported.pdf");
        let pixels = image::RgbaImage::from_raw(2, 1, vec![255, 0, 0, 128, 0, 0, 255, 255]).unwrap(); pixels.save_with_format(&source, image::ImageFormat::Png).unwrap(); let source_bytes = std::fs::read(&source).unwrap();
        let existing = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); let before = call(&service, |reply| Request::Render(existing.id, 0, 128, reply)).unwrap();
        let options = crate::image_pdf::ImagePdfOptions { page_size: crate::image_pdf::ImagePdfPageSize::Letter, orientation: crate::image_pdf::ImagePdfOrientation::Auto, margin_points: 12.0 };
        let saved = call(&service, |reply| Request::CreateImagePdf(source.clone(), output.clone(), options, reply)).unwrap();
        assert_eq!(saved.path, output.to_string_lossy()); assert_eq!(saved.document.pages.len(), 1); assert_eq!((saved.document.pages[0].width, saved.document.pages[0].height), (792.0, 612.0)); assert_eq!(saved.document.revision, 0); assert!(!saved.document.dirty && !saved.document.can_undo && !saved.document.can_redo);
        let preview = call(&service, |reply| Request::Render(saved.document.id, 0, 128, reply)).unwrap(); assert!(preview.starts_with(b"\x89PNG\r\n\x1a\n"));
        let snapshot = call(&service, |reply| Request::BeginPrint(saved.document.id, 0, reply)).unwrap(); let printed = service.print_render_blocking(snapshot.token, 0, 128, 128).unwrap(); assert!(printed.width > 0 && printed.height > 0 && !printed.bgra.is_empty()); call(&service, |reply| Request::EndPrint(snapshot.token, reply)).unwrap(); std::mem::forget(snapshot);
        call(&service, |reply| Request::Save(saved.document.id, None, exported.clone(), reply)).unwrap(); let reopened = call(&service, |reply| Request::Open(exported.clone(), reply)).unwrap(); assert_eq!(reopened.pages.len(), 1); assert_eq!(call(&service, |reply| Request::Render(reopened.id, 0, 128, reply)).unwrap(), preview);
        assert_eq!(std::fs::read(&source).unwrap(), source_bytes); assert_eq!(call(&service, |reply| Request::Render(existing.id, 0, 128, reply)).unwrap(), before); assert_eq!(call(&service, |reply| Request::Properties(existing.id, 0, reply)).unwrap().page_count, 6);
        for id in [saved.document.id, reopened.id, existing.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
    }
    #[test]
    fn unread_image_pdf_reply_releases_session_and_preserves_published_output() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = folder.path().join("source.png"); let output = folder.path().join("created.pdf"); image::RgbImage::from_raw(1, 1, vec![10, 20, 30]).unwrap().save(&source).unwrap(); let source_bytes = std::fs::read(&source).unwrap();
        let options = crate::image_pdf::ImagePdfOptions { page_size: crate::image_pdf::ImagePdfPageSize::A4, orientation: crate::image_pdf::ImagePdfOrientation::Portrait, margin_points: 0.0 };
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![output.clone()] };
        let retained = unread_document_reply(&service, &output, |reply| Request::CreateImagePdf(source.clone(), output.clone(), options, reply));
        assert!(retained.is_empty(), "Successfully sent but unread CreateImagePdf retained {} session(s)", retained.len()); assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
        assert_eq!(lopdf::Document::load(&output).unwrap().get_pages().len(), 1); let reopened = call(&service, |reply| Request::Open(output.clone(), reply)).unwrap(); assert_eq!(reopened.pages.len(), 1); call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
    }
    #[test]
    fn unread_password_required_reply_releases_pending_challenge() {
        let _password_lock = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("password.pdf"); let source = password_source(&root); std::fs::write(&path, &source).unwrap();
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![path.clone()] };
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginOpen(path.clone(), tx)).unwrap();
        let pending = call(&service, |reply| Request::PasswordRequestsForPath(path.clone(), reply)).unwrap(); assert_eq!(pending.len(), 1);
        assert!(!rx.is_empty(), "The challenge reply must be sent before its unread receiver is dropped"); drop(rx);
        let retained = call(&service, |reply| Request::PasswordRequestsForPath(path.clone(), reply)).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), source);
        assert!(retained.is_empty(), "Successfully sent but unread PasswordRequired retained {} pending token(s)", retained.len());
    }
    #[test]
    fn accepted_document_replies_and_password_challenges_remain_usable_after_dto_drop() {
        let _password_lock = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let first_path = folder.path().join("first.pdf"); let second_path = folder.path().join("second.pdf"); let password_path = folder.path().join("password.pdf"); let output = folder.path().join("combined.pdf");
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); std::fs::write(&first_path, &source).unwrap(); std::fs::write(&second_path, &source).unwrap(); std::fs::write(&password_path, password_source(&root)).unwrap();
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![first_path.clone(), second_path.clone(), password_path.clone(), output.clone()] };
        let first = tauri::async_runtime::block_on(service.open(first_path)).unwrap(); let first_source = combine_source(&first); drop(first);
        let second = tauri::async_runtime::block_on(service.begin_open(second_path)).unwrap();
        let second_source = match &second { OpenResult::Opened { document, .. } => combine_source(document), _ => panic!("Ordinary PDF must open") }; drop(second);
        for id in [first_source.id, second_source.id] { assert_eq!(call(&service, |reply| Request::Properties(id, 0, reply)).unwrap().page_count, 6); }
        let challenge = tauri::async_runtime::block_on(service.begin_open(password_path.clone())).unwrap();
        let request_id = match &challenge { OpenResult::PasswordRequired { request_id, .. } => *request_id, _ => panic!("Expected password challenge") }; drop(challenge);
        let wrong = tauri::async_runtime::block_on(service.unlock(request_id, "wrong".into())).unwrap(); assert!(matches!(wrong, OpenResult::PasswordRequired { incorrect: true, .. })); drop(wrong);
        let opened = tauri::async_runtime::block_on(service.unlock(request_id, "test password".into())).unwrap();
        let unlocked_id = match &opened { OpenResult::Opened { document, .. } => document.id, _ => panic!("Correct password must open") }; drop(opened);
        assert_eq!(call(&service, |reply| Request::Properties(unlocked_id, 0, reply)).unwrap().page_count, 6);
        let text = call(&service, |reply| Request::Text(unlocked_id, 0, 0, reply)).unwrap(); assert!(text.contains("Page 1 of 6"));
        let challenge = tauri::async_runtime::block_on(service.begin_open(password_path.clone())).unwrap();
        let canceled = match &challenge { OpenResult::PasswordRequired { request_id, .. } => *request_id, _ => panic!("Expected password challenge") }; drop(challenge);
        tauri::async_runtime::block_on(service.cancel_password(canceled)).unwrap(); assert!(tauri::async_runtime::block_on(service.unlock(canceled, "test password".into())).is_err());
        let combined = tauri::async_runtime::block_on(service.combine(first_source, second_source, output.clone())).unwrap(); let combined_id = combined.document.id;
        assert!(!combined.document.dirty && !combined.document.can_undo && !combined.document.can_redo); drop(combined);
        assert_eq!(call(&service, |reply| Request::Properties(combined_id, 0, reply)).unwrap().page_count, 12);
        assert!(call(&service, |reply| Request::Text(combined_id, 11, 0, reply)).unwrap().contains("Page 6 of 6"));
        assert_eq!(lopdf::Document::load(output).unwrap().get_pages().len(), 12);
    }
    #[test]
    fn unread_preclosed_open_begin_unlock_and_combine_replies_never_retain_resources_or_publish() {
        let _password_lock = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("ordinary.pdf"); let password_path = folder.path().join("password.pdf"); let output = folder.path().join("must-not-publish.pdf");
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); let encrypted = password_source(&root); std::fs::write(&path, &source).unwrap(); std::fs::write(&password_path, &encrypted).unwrap();
        let _cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![path.clone(), password_path.clone(), output.clone()] };
        let (tx, rx) = oneshot::channel(); drop(rx); service.sender.send(Request::Open(path.clone(), tx)).unwrap();
        let (tx, rx) = oneshot::channel(); drop(rx); service.sender.send(Request::BeginOpen(password_path.clone(), tx)).unwrap();
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(path.clone(), reply)).unwrap().is_empty());
        assert!(call(&service, |reply| Request::PasswordRequestsForPath(password_path.clone(), reply)).unwrap().is_empty());
        let challenge = call(&service, |reply| Request::BeginOpen(password_path.clone(), reply)).unwrap(); let request_id = match challenge { OpenResult::PasswordRequired { request_id, .. } => request_id, _ => panic!("Expected password challenge") };
        let (tx, rx) = oneshot::channel(); drop(rx); service.sender.send(Request::Unlock(request_id, "test password".into(), tx)).unwrap();
        assert!(call(&service, |reply| Request::PasswordRequestsForPath(password_path.clone(), reply)).unwrap().is_empty());
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(password_path.clone(), reply)).unwrap().is_empty());
        let first = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let second = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        let (tx, rx) = oneshot::channel(); drop(rx); service.sender.send(Request::Combine(combine_source(&first), combine_source(&second), output.clone(), tx)).unwrap();
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(output.clone(), reply)).unwrap().is_empty()); assert!(!output.exists());
        assert_eq!(std::fs::read(path).unwrap(), source); assert_eq!(std::fs::read(password_path).unwrap(), encrypted);
    }
    #[test]
    fn print_snapshot_lease_survives_thread_handoff_source_close_and_releases_on_completion_or_unwind() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let reusable = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        for unwind in [false, true] {
            let info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
            let snapshot = call(&service, |reply| Request::BeginPrint(info.id, info.revision, reply)).unwrap(); let token = snapshot.token;
            let expected = service.print_render_blocking(token, 0, 100, 100).unwrap().bgra;
            let (ready, started) = std::sync::mpsc::channel(); let (finish, wait) = std::sync::mpsc::channel(); let worker = service.clone();
            let thread = std::thread::spawn(move || {
                let snapshot = snapshot;
                ready.send(()).unwrap(); wait.recv().unwrap();
                let rendered = worker.print_render_blocking(snapshot.token, 0, 100, 100).unwrap().bgra;
                if unwind { panic!("Exercise snapshot lease cleanup during spool-thread unwind"); }
                rendered
            });
            started.recv().unwrap();
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
            assert_eq!(service.print_render_blocking(token, 0, 100, 100).unwrap().bgra, expected);
            let held = (0..3).map(|_| print_snapshot(&service, reusable.id, 0)).collect::<Vec<_>>();
            assert!(call(&service, |reply| Request::BeginPrint(reusable.id, 0, reply)).err().unwrap().contains("another print job"), "The active spool lease must retain its admission slot");
            finish.send(()).unwrap(); let result = thread.join();
            if unwind { assert!(result.is_err()); } else { assert_eq!(result.unwrap(), expected); }
            call(&service, |reply| Request::Properties(reusable.id, 0, reply)).unwrap();
            assert!(service.print_render_blocking(token, 0, 100, 100).err().unwrap().contains("ended"));
            let replacement = print_snapshot(&service, reusable.id, 0);
            assert!(call(&service, |reply| Request::BeginPrint(reusable.id, 0, reply)).err().unwrap().contains("another print job"));
            drop(replacement); drop(held);
        }
        call(&service, |reply| Request::Close(reusable.id, reply)).unwrap();
    }
    #[test]
    fn print_snapshot_admission_limit_and_guard_cleanup_preserve_worker_capacity() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let held = (0..3).map(|_| print_snapshot(&service, info.id, 0)).collect::<Vec<_>>();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _fourth = print_snapshot(&service, info.id, 0);
            assert!(call(&service, |reply| Request::BeginPrint(info.id, 0, reply)).err().unwrap().contains("another print job"));
            panic!("Exercise snapshot guard cleanup after a failed assertion");
        }));
        assert!(unwind.is_err());
        let replacement = print_snapshot(&service, info.id, 0);
        assert!(call(&service, |reply| Request::BeginPrint(info.id, 0, reply)).err().unwrap().contains("another print job"));
        drop(replacement); drop(held);
        let reusable = (0..4).map(|_| print_snapshot(&service, info.id, 0)).collect::<Vec<_>>();
        assert!(call(&service, |reply| Request::BeginPrint(info.id, 0, reply)).err().unwrap().contains("another print job"));
        drop(reusable);
        call(&service, |reply| Request::Close(info.id, reply)).unwrap();
    }
    fn assert_same_ink(actual: &image::RgbImage, expected: &image::RgbImage) {
        assert!(actual.width().abs_diff(expected.width()) <= 1 && actual.height().abs_diff(expected.height()) <= 1, "Unexpected crop dimensions: {:?} vs {:?}", actual.dimensions(), expected.dimensions());
        let ink = |image: &image::RgbImage| image.enumerate_pixels().filter(|(_, _, pixel)| pixel.0.iter().any(|value| *value < 100)).map(|(x, y, _)| (x, y)).collect::<Vec<_>>();
        let actual_ink = ink(actual); let expected_ink = ink(expected);
        assert!(!actual_ink.is_empty() && !expected_ink.is_empty());
        let compare = |points: &[(u32, u32)], source: &image::RgbImage, target: &image::RgbImage| {
            points.iter().filter(|(x, y)| {
                let x = (f64::from(*x) * f64::from(target.width()) / f64::from(source.width())).round() as i32;
                let y = (f64::from(*y) * f64::from(target.height()) / f64::from(source.height())).round() as i32;
                (-2..=2).any(|dy| (-2..=2).any(|dx| {
                    let (x, y) = (x + dx, y + dy);
                    x >= 0 && y >= 0 && x < target.width() as i32 && y < target.height() as i32 && target.get_pixel(x as u32, y as u32).0.iter().any(|value| *value < 100)
                }))
            }).count()
        };
        assert!(compare(&actual_ink, actual, expected) as f64 / actual_ink.len() as f64 > 0.99, "Crop has unexpected rendered ink");
        assert!(compare(&expected_ink, expected, actual) as f64 / expected_ink.len() as f64 > 0.99, "Crop lost expected rendered ink");
    }
    fn combine_source(info: &DocumentInfo) -> CombineSource { CombineSource { id: info.id, revision: info.revision } }
    struct TestDocument { service: PdfService, id: u64 }
    impl Drop for TestDocument {
        fn drop(&mut self) {
            let (tx, rx) = oneshot::channel();
            if self.service.sender.send(Request::Close(self.id, tx)).is_ok() { let _ = rx.blocking_recv(); }
        }
    }
    #[test]
    fn replace_all_ordered_fixture_pairs_validate_every_output_page_and_preserve_source_history() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let fixtures = [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)];
        for (target_index, (target_fixture, target_count)) in fixtures.iter().enumerate() {
            for (donor_index, (donor_fixture, donor_count)) in fixtures.iter().enumerate() {
                if target_index == donor_index { continue; }
                let target_path = root.join(target_fixture); let donor_path = root.join(donor_fixture);
                let target_bytes = std::fs::read(&target_path).unwrap(); let donor_bytes = std::fs::read(&donor_path).unwrap();
                let mut target = call(&service, |reply| Request::Open(target_path.clone(), reply)).unwrap(); let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
                let mut donor = call(&service, |reply| Request::Open(donor_path.clone(), reply)).unwrap(); let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
                for edit in [PageEdit::Move { from: 0, to: 2 }, PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] { target = call(&service, |reply| Request::Edit(target.id, edit, reply)).unwrap(); }
                for edit in [PageEdit::Delete { pages: vec![1, 3] }, PageEdit::Rotate { pages: vec![0], clockwise: false }, PageEdit::Move { from: 0, to: 1 }] { donor = call(&service, |reply| Request::Edit(donor.id, edit, reply)).unwrap(); }
                let count = 10.min(target.pages.len() - 2); let start = (target.pages.len() - count) / 2;
                let path = folder.path().join(format!("replace-{target_count}-{donor_count}.pdf"));
                call(&service, |reply| Request::CheckReplacement(combine_source(&target), combine_source(&donor), start, count, reply)).unwrap();
                let started = std::time::Instant::now();
                let replaced = call(&service, |reply| Request::ReplacePages(combine_source(&target), combine_source(&donor), start, count, path.clone(), reply)).unwrap().document;
                let _replaced_cleanup = TestDocument { service: service.clone(), id: replaced.id };
                println!("replace {target_count}+{donor_count} start={start} count={count}: {} current pages, validation/write {:?}", replaced.pages.len(), started.elapsed());
                assert_ne!(replaced.id, target.id); assert_ne!(replaced.id, donor.id); assert_eq!(replaced.pages.len(), target_count + donor_count - 3 - count);
                assert_eq!(replaced.revision, 0); assert!(!replaced.dirty && !replaced.can_undo && !replaced.can_redo);
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let _reopened_cleanup = TestDocument { service: service.clone(), id: reopened.id };
                for position in 0..replaced.pages.len() {
                    let (source, local, source_count, is_target) = if position < start { (&target, position, target_count, true) }
                        else if position < start + donor.pages.len() { (&donor, position - start, donor_count, false) }
                        else { (&target, position - donor.pages.len() + count, target_count, true) };
                    let original = if is_target { match local { 0 => 1, 1 => 0, other => other + 1 } } else { match local { 0 => 2, 1 => 0, other => other + 2 } };
                    let footer = format!("Page {} of {source_count}", original + 1);
                    assert_eq!((replaced.pages[position].width, replaced.pages[position].height), (source.pages[local].width, source.pages[local].height));
                    let expected = image::load_from_memory(&call(&service, |reply| Request::Render(source.id, local as u16, 64, reply)).unwrap()).unwrap().into_rgba8();
                    for id in [replaced.id, reopened.id] {
                        assert!(call(&service, |reply| Request::Text(id, position as u16, 0, reply)).unwrap().contains(&footer), "Replacement source/order differs at output page {position}");
                        let actual = image::load_from_memory(&call(&service, |reply| Request::Render(id, position as u16, 64, reply)).unwrap()).unwrap().into_rgba8();
                        assert_eq!(actual, expected, "Replacement/reopened render differs at output page {position}");
                    }
                }
                assert_eq!(std::fs::read(target_path).unwrap(), target_bytes); assert_eq!(std::fs::read(donor_path).unwrap(), donor_bytes);
                for source in [&target, &donor] {
                    assert_eq!(call(&service, |reply| Request::Properties(source.id, source.revision, reply)).unwrap().page_count, source.pages.len());
                    for undo in 0..3 {
                        let restored = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap();
                        assert_eq!(restored.revision, source.revision + undo + 1); assert_eq!(restored.dirty, undo != 2); assert!(restored.can_redo);
                    }
                }
            }
        }
    }
    #[test]
    fn replace_duplicate_invalid_stale_and_closed_after_preflight_never_write_or_mutate_sources() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let mut target = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
        let mut donor = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
        let path = folder.path().join("must-not-replace.pdf");
        let replace = |target, donor, start, count| call(&service, |reply| Request::ReplacePages(target, donor, start, count, path.clone(), reply));
        assert!(replace(combine_source(&target), combine_source(&target), 0, 1).err().unwrap().contains("different"));
        for (start,count) in [(0,0),(6,1),(5,2),(usize::MAX,1),(1,usize::MAX)] { assert!(replace(combine_source(&target), combine_source(&donor), start, count).err().unwrap().contains("range")); }
        assert!(serde_json::from_value::<usize>(serde_json::json!(0.5)).is_err()); assert!(serde_json::from_value::<usize>(serde_json::json!(-1)).is_err());
        assert_eq!(call(&service, |reply| Request::Properties(target.id, 0, reply)).unwrap().page_count, 6);
        assert_eq!(call(&service, |reply| Request::Properties(donor.id, 0, reply)).unwrap().page_count, 6);
        call(&service, |reply| Request::CheckReplacement(combine_source(&target), combine_source(&donor), 2, 2, reply)).unwrap();
        let old_donor = combine_source(&donor); donor = call(&service, |reply| Request::Edit(donor.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        assert!(replace(combine_source(&target), old_donor, 2, 2).err().unwrap().contains("changed"));
        call(&service, |reply| Request::CheckReplacement(combine_source(&target), combine_source(&donor), 2, 2, reply)).unwrap();
        let old_target = combine_source(&target); target = call(&service, |reply| Request::Edit(target.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        assert!(replace(old_target, combine_source(&donor), 2, 2).err().unwrap().contains("changed"));
        call(&service, |reply| Request::CheckReplacement(combine_source(&target), combine_source(&donor), 2, 2, reply)).unwrap();
        call(&service, |reply| Request::Close(donor.id, reply)).unwrap(); assert!(replace(combine_source(&target), combine_source(&donor), 2, 2).err().unwrap().contains("Donor document is closed"));
        call(&service, |reply| Request::Close(target.id, reply)).unwrap(); assert!(replace(combine_source(&target), combine_source(&donor), 2, 2).err().unwrap().contains("Target document is closed"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn replace_cropped_inherited_original_and_edited_rotations_keep_independent_ink_and_visible_tokens() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for original in [0, 90, 180, 270] {
            for edited in 0..4 {
                let mut sources = Vec::new(); let mut expected = Vec::new(); let mut cleanup = Vec::new();
                for (index, label) in ["Target", "Donor"].into_iter().enumerate() {
                    let rotation = if index == 0 { original } else { (360 - original) % 360 };
                    let bytes = crate::text_geometry::tests::fixture(rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 300.0], &format!("{label}OutsideTop\n\n{label}Header\n{label}Body\n\n{label}OutsideBottom"));
                    let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
                    pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]);
                    let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
                    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] { let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value); }
                    let visible_page = if index == 0 {
                        let retained = pdf.add_object(pdf.get_dictionary(page).unwrap().clone());
                        let removed = pdf.get_page_content(page); let removed = String::from_utf8(removed).unwrap().replace("Target", "Removed").into_bytes();
                        let removed = pdf.add_object(lopdf::Stream::new(lopdf::Dictionary::new(), removed)); pdf.get_dictionary_mut(page).unwrap().set("Contents", removed);
                        pdf.get_dictionary_mut(parent).unwrap().set("Kids", vec![lopdf::Object::Reference(page), lopdf::Object::Reference(retained)]);
                        pdf.get_dictionary_mut(parent).unwrap().set("Count", 2); 1
                    } else { 0 };
                    let path = folder.path().join(format!("replace-source-{original}-{edited}-{index}.pdf")); pdf.save(&path).unwrap(); let source_bytes = std::fs::read(&path).unwrap();
                    let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); cleanup.push(TestDocument { service: service.clone(), id: info.id });
                    for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: if index == 0 { vec![0,1] } else { vec![0] }, clockwise: index == 0 }, reply)).unwrap(); }
                    let turns = (rotation / 90 + if index == 0 { edited } else { (4 - edited) % 4 }) % 4;
                    let (rect, region, old_width, new_width) = match turns {
                        0 => (CropRect { x: 0.1, y: 70.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 210, 720, 330), 900, 720),
                        1 => (CropRect { x: 80.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (240, 90, 330, 720), 780, 330),
                        2 => (CropRect { x: 0.1, y: 80.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 240, 720, 330), 900, 720),
                        _ => (CropRect { x: 70.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (210, 90, 330, 720), 780, 330),
                    };
                    let before = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, visible_page, old_width, reply)).unwrap()).unwrap().into_rgb8();
                    expected.push((image::imageops::crop_imm(&before, region.0, region.1, region.2, region.3).to_image(), new_width, turns));
                    info = call(&service, |reply| Request::Crop(info.id, visible_page, info.revision, rect, reply)).unwrap(); sources.push((info, path, source_bytes));
                }
                let path = folder.path().join(format!("replace-cropped-{original}-{edited}.pdf"));
                let replaced = call(&service, |reply| Request::ReplacePages(combine_source(&sources[0].0), combine_source(&sources[1].0), 0, 1, path.clone(), reply)).unwrap().document;
                cleanup.push(TestDocument { service: service.clone(), id: replaced.id }); assert_eq!(replaced.pages.len(), 2);
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); cleanup.push(TestDocument { service: service.clone(), id: reopened.id });
                let output = lopdf::Document::load(&path).unwrap();
                for (position, (source_index, label)) in [(1usize, "Donor"), (0, "Target")].into_iter().enumerate() {
                    let (expected, width, turns) = &expected[source_index]; let dimensions = if turns % 2 == 0 { (240.0, 110.0) } else { (110.0, 240.0) };
                    assert_eq!((replaced.pages[position].width, replaced.pages[position].height), dimensions);
                    for id in [replaced.id, reopened.id] {
                        let actual = image::load_from_memory(&call(&service, |reply| Request::Render(id, position as u16, *width, reply)).unwrap()).unwrap().into_rgb8(); assert_same_ink(&actual, expected);
                        let text = call(&service, |reply| Request::Text(id, position as u16, 0, reply)).unwrap();
                        let geometry = call(&service, |reply| Request::TextGeometry(id, position as u16, 0, reply)).unwrap(); assert_eq!(geometry.status, "ok");
                        let copied = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
                        for token in [format!("{label}Header"), format!("{label}Body")] { assert!(text.contains(&token), "{original}/{edited}: {text}"); assert!(copied.contains(&token), "{original}/{edited}: {copied}"); }
                        for token in [format!("{label}OutsideTop"), format!("{label}OutsideBottom"), "RemovedHeader".into(), "RemovedBody".into()] { assert!(!text.contains(&token) && !copied.contains(&token)); }
                    }
                    assert!(String::from_utf8_lossy(&output.get_page_content(output.get_pages()[&(position as u32 + 1)])).contains(&format!("{label}OutsideBottom")), "Replacement must preserve underlying crop-hidden content");
                    let (source, source_path, source_bytes) = &sources[source_index]; assert_eq!(std::fs::read(source_path).unwrap(), *source_bytes);
                    let undo = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(undo.revision, source.revision + 1); assert!(undo.can_redo);
                }
            }
        }
    }
    #[test]
    fn replace_unread_and_preclosed_replies_preserve_publication_and_accepted_copy_owns_its_source() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); let target_path = folder.path().join("target.pdf"); let donor_path = folder.path().join("donor.pdf");
        std::fs::write(&target_path, &source).unwrap(); std::fs::write(&donor_path, &source).unwrap();
        let target = call(&service, |reply| Request::Open(target_path.clone(), reply)).unwrap(); let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
        let donor = call(&service, |reply| Request::Open(donor_path.clone(), reply)).unwrap(); let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
        let target_print = print_snapshot(&service, target.id, target.revision); let donor_print = print_snapshot(&service, donor.id, donor.revision);
        let original_print = service.print_render_blocking(target_print.token, 0, 64, 64).unwrap().bgra;
        let canceled = folder.path().join("canceled.pdf"); let (tx, rx) = oneshot::channel(); drop(rx);
        service.sender.send(Request::ReplacePages(combine_source(&target), combine_source(&donor), 0, 1, canceled.clone(), tx)).unwrap();
        call(&service, |reply| Request::Properties(target.id, target.revision, reply)).unwrap(); assert!(!canceled.exists());
        let output = folder.path().join("unread.pdf"); let _unread_cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![output.clone()] };
        let retained = unread_document_reply(&service, &output, |reply| Request::ReplacePages(combine_source(&target), combine_source(&donor), 2, 2, output.clone(), reply));
        assert!(retained.is_empty(), "Unread replacement result must release only the new session"); assert!(output.exists());
        let reopened = call(&service, |reply| Request::Open(output.clone(), reply)).unwrap(); let _reopened_cleanup = TestDocument { service: service.clone(), id: reopened.id }; assert_eq!(reopened.pages.len(), 10);
        let accepted_path = folder.path().join("accepted.pdf");
        let accepted = tauri::async_runtime::block_on(service.replace_pages_copy(combine_source(&target), combine_source(&donor), 0, 6, accepted_path.clone())).unwrap();
        assert_eq!(accepted.path, accepted_path.to_string_lossy()); assert_eq!(accepted.document.path, accepted.path); assert_eq!(accepted.document.pages.len(), 6);
        assert!(!accepted.document.dirty && !accepted.document.can_undo && !accepted.document.can_redo);
        let accepted_id = accepted.document.id; drop(accepted); let _accepted_cleanup = TestDocument { service: service.clone(), id: accepted_id };
        assert_eq!(call(&service, |reply| Request::Properties(accepted_id, 0, reply)).unwrap().page_count, 6);
        assert_eq!(std::fs::read(target_path).unwrap(), source); assert_eq!(std::fs::read(donor_path).unwrap(), source);
        call(&service, |reply| Request::Close(target.id, reply)).unwrap(); call(&service, |reply| Request::Close(donor.id, reply)).unwrap();
        assert_eq!(service.print_render_blocking(target_print.token, 0, 64, 64).unwrap().bgra, original_print);
        assert_eq!(service.print_render_blocking(donor_print.token, 0, 64, 64).unwrap().bgra, original_print);
        assert!(call(&service, |reply| Request::Text(accepted_id, 5, 0, reply)).unwrap().contains("Page 6 of 6"));
        assert!(!call(&service, |reply| Request::Render(accepted_id, 5, 64, reply)).unwrap().is_empty());
    }
    #[test]
    fn insert_all_ordered_fixture_pairs_validate_every_output_page_and_preserve_source_history() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let fixtures = [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)];
        for (target_index, (target_fixture, target_count)) in fixtures.iter().enumerate() {
            for (donor_index, (donor_fixture, donor_count)) in fixtures.iter().enumerate() {
                if target_index == donor_index { continue; }
                let target_path = root.join(target_fixture); let donor_path = root.join(donor_fixture);
                let target_bytes = std::fs::read(&target_path).unwrap(); let donor_bytes = std::fs::read(&donor_path).unwrap();
                let mut target = call(&service, |reply| Request::Open(target_path.clone(), reply)).unwrap();
                let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
                let mut donor = call(&service, |reply| Request::Open(donor_path.clone(), reply)).unwrap();
                let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
                for edit in [PageEdit::Move { from: 0, to: 2 }, PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] { target = call(&service, |reply| Request::Edit(target.id, edit, reply)).unwrap(); }
                for edit in [PageEdit::Delete { pages: vec![1, 3] }, PageEdit::Rotate { pages: vec![0], clockwise: false }, PageEdit::Move { from: 0, to: 1 }] { donor = call(&service, |reply| Request::Edit(donor.id, edit, reply)).unwrap(); }
                let at = target.pages.len() / 2; let path = folder.path().join(format!("insert-{target_count}-{donor_count}.pdf"));
                call(&service, |reply| Request::CheckInsertion(combine_source(&target), combine_source(&donor), at, reply)).unwrap();
                let started = std::time::Instant::now();
                let inserted = call(&service, |reply| Request::InsertPages(combine_source(&target), combine_source(&donor), at, path.clone(), reply)).unwrap().document;
                let _inserted_cleanup = TestDocument { service: service.clone(), id: inserted.id };
                println!("insert {target_count}+{donor_count} at={at}: {} current pages, validation/write {:?}", inserted.pages.len(), started.elapsed());
                assert_ne!(inserted.id, target.id); assert_ne!(inserted.id, donor.id); assert_eq!(inserted.pages.len(), target_count + donor_count - 3);
                assert_eq!(inserted.revision, 0); assert!(!inserted.dirty && !inserted.can_undo && !inserted.can_redo);
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); let _reopened_cleanup = TestDocument { service: service.clone(), id: reopened.id };
                for position in 0..inserted.pages.len() {
                    let (source, local, count, is_target) = if position < at { (&target, position, target_count, true) }
                        else if position < at + donor.pages.len() { (&donor, position - at, donor_count, false) }
                        else { (&target, position - donor.pages.len(), target_count, true) };
                    let original = if is_target { match local { 0 => 1, 1 => 0, other => other + 1 } } else { match local { 0 => 2, 1 => 0, other => other + 2 } };
                    let footer = format!("Page {} of {count}", original + 1);
                    assert_eq!((inserted.pages[position].width, inserted.pages[position].height), (source.pages[local].width, source.pages[local].height));
                    let expected = image::load_from_memory(&call(&service, |reply| Request::Render(source.id, local as u16, 64, reply)).unwrap()).unwrap().into_rgba8();
                    for id in [inserted.id, reopened.id] {
                        assert!(call(&service, |reply| Request::Text(id, position as u16, 0, reply)).unwrap().contains(&footer), "Inserted source/order differs at output page {position}");
                        let actual = image::load_from_memory(&call(&service, |reply| Request::Render(id, position as u16, 64, reply)).unwrap()).unwrap().into_rgba8();
                        assert_eq!(actual, expected, "Inserted/reopened render differs at output page {position}");
                    }
                }
                assert_eq!(std::fs::read(target_path).unwrap(), target_bytes); assert_eq!(std::fs::read(donor_path).unwrap(), donor_bytes);
                for source in [&target, &donor] {
                    assert_eq!(call(&service, |reply| Request::Properties(source.id, source.revision, reply)).unwrap().page_count, source.pages.len());
                    for undo in 0..3 {
                        let restored = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap();
                        assert_eq!(restored.revision, source.revision + undo + 1); assert_eq!(restored.dirty, undo != 2); assert!(restored.can_redo);
                    }
                }
            }
        }
    }
    #[test]
    fn insert_duplicate_invalid_stale_and_closed_after_preflight_never_write_or_mutate_sources() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let mut target = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
        let mut donor = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap(); let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
        let path = folder.path().join("must-not-insert.pdf");
        let insert = |target, donor, at| call(&service, |reply| Request::InsertPages(target, donor, at, path.clone(), reply));
        assert!(insert(combine_source(&target), combine_source(&target), 0).err().unwrap().contains("different"));
        assert!(insert(combine_source(&target), combine_source(&donor), 7).err().unwrap().contains("position"));
        assert!(serde_json::from_value::<usize>(serde_json::json!(0.5)).is_err()); assert!(serde_json::from_value::<usize>(serde_json::json!(-1)).is_err());
        call(&service, |reply| Request::CheckInsertion(combine_source(&target), combine_source(&donor), 3, reply)).unwrap();
        let old_donor = combine_source(&donor); donor = call(&service, |reply| Request::Edit(donor.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        assert!(insert(combine_source(&target), old_donor, 3).err().unwrap().contains("changed"));
        call(&service, |reply| Request::CheckInsertion(combine_source(&target), combine_source(&donor), 3, reply)).unwrap();
        let old_target = combine_source(&target); target = call(&service, |reply| Request::Edit(target.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        assert!(insert(old_target, combine_source(&donor), 3).err().unwrap().contains("changed"));
        call(&service, |reply| Request::CheckInsertion(combine_source(&target), combine_source(&donor), 3, reply)).unwrap();
        call(&service, |reply| Request::Close(donor.id, reply)).unwrap(); assert!(insert(combine_source(&target), combine_source(&donor), 3).err().unwrap().contains("Donor document is closed"));
        call(&service, |reply| Request::Close(target.id, reply)).unwrap(); assert!(insert(combine_source(&target), combine_source(&donor), 3).err().unwrap().contains("Target document is closed"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn insert_cropped_inherited_original_and_edited_rotations_keep_independent_ink_and_visible_tokens() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for original in [0, 90, 180, 270] {
            for edited in 0..4 {
                let mut sources = Vec::new(); let mut expected = Vec::new(); let mut cleanup = Vec::new();
                for (index, label) in ["Target", "Donor"].into_iter().enumerate() {
                    let rotation = if index == 0 { original } else { (360 - original) % 360 };
                    let bytes = crate::text_geometry::tests::fixture(rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 300.0], &format!("{label}OutsideTop\n\n{label}Header\n{label}Body\n\n{label}OutsideBottom"));
                    let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
                    pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]);
                    let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
                    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] { let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value); }
                    let path = folder.path().join(format!("insert-source-{original}-{edited}-{index}.pdf")); pdf.save(&path).unwrap(); let source_bytes = std::fs::read(&path).unwrap();
                    let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); cleanup.push(TestDocument { service: service.clone(), id: info.id });
                    for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: index == 0 }, reply)).unwrap(); }
                    let turns = (rotation / 90 + if index == 0 { edited } else { (4 - edited) % 4 }) % 4;
                    let (rect, region, old_width, new_width) = match turns {
                        0 => (CropRect { x: 0.1, y: 70.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 210, 720, 330), 900, 720),
                        1 => (CropRect { x: 80.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (240, 90, 330, 720), 780, 330),
                        2 => (CropRect { x: 0.1, y: 80.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 240, 720, 330), 900, 720),
                        _ => (CropRect { x: 70.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (210, 90, 330, 720), 780, 330),
                    };
                    let before = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, old_width, reply)).unwrap()).unwrap().into_rgb8();
                    expected.push((image::imageops::crop_imm(&before, region.0, region.1, region.2, region.3).to_image(), new_width, turns));
                    info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, rect, reply)).unwrap(); sources.push((info, path, source_bytes));
                }
                let path = folder.path().join(format!("insert-cropped-{original}-{edited}.pdf"));
                let inserted = call(&service, |reply| Request::InsertPages(combine_source(&sources[0].0), combine_source(&sources[1].0), 0, path.clone(), reply)).unwrap().document;
                cleanup.push(TestDocument { service: service.clone(), id: inserted.id });
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); cleanup.push(TestDocument { service: service.clone(), id: reopened.id });
                let output = lopdf::Document::load(&path).unwrap();
                for (position, (source_index, label)) in [(1usize, "Donor"), (0, "Target")].into_iter().enumerate() {
                    let (expected, width, turns) = &expected[source_index]; let dimensions = if turns % 2 == 0 { (240.0, 110.0) } else { (110.0, 240.0) };
                    assert_eq!((inserted.pages[position].width, inserted.pages[position].height), dimensions);
                    for id in [inserted.id, reopened.id] {
                        let actual = image::load_from_memory(&call(&service, |reply| Request::Render(id, position as u16, *width, reply)).unwrap()).unwrap().into_rgb8(); assert_same_ink(&actual, expected);
                        let text = call(&service, |reply| Request::Text(id, position as u16, 0, reply)).unwrap();
                        let geometry = call(&service, |reply| Request::TextGeometry(id, position as u16, 0, reply)).unwrap(); assert_eq!(geometry.status, "ok");
                        let copied = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
                        for token in [format!("{label}Header"), format!("{label}Body")] { assert!(text.contains(&token), "{original}/{edited}: {text}"); assert!(copied.contains(&token), "{original}/{edited}: {copied}"); }
                        for token in [format!("{label}OutsideTop"), format!("{label}OutsideBottom")] { assert!(!text.contains(&token) && !copied.contains(&token)); }
                    }
                    assert!(String::from_utf8_lossy(&output.get_page_content(output.get_pages()[&(position as u32 + 1)])).contains(&format!("{label}OutsideBottom")), "Insertion must preserve underlying crop-hidden content");
                    let (source, source_path, source_bytes) = &sources[source_index]; assert_eq!(std::fs::read(source_path).unwrap(), *source_bytes);
                    let undo = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(undo.revision, source.revision + 1); assert!(undo.can_redo);
                }
            }
        }
    }
    #[test]
    fn insert_unread_and_preclosed_replies_preserve_publication_and_accepted_copy_owns_its_source() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let source = std::fs::read(root.join("resources/welcome.pdf")).unwrap(); let target_path = folder.path().join("target.pdf"); let donor_path = folder.path().join("donor.pdf");
        std::fs::write(&target_path, &source).unwrap(); std::fs::write(&donor_path, &source).unwrap();
        let target = call(&service, |reply| Request::Open(target_path.clone(), reply)).unwrap(); let _target_cleanup = TestDocument { service: service.clone(), id: target.id };
        let donor = call(&service, |reply| Request::Open(donor_path.clone(), reply)).unwrap(); let _donor_cleanup = TestDocument { service: service.clone(), id: donor.id };
        let target_print = print_snapshot(&service, target.id, target.revision); let donor_print = print_snapshot(&service, donor.id, donor.revision);
        let original_print = service.print_render_blocking(target_print.token, 0, 64, 64).unwrap().bgra;
        let canceled = folder.path().join("canceled.pdf"); let (tx, rx) = oneshot::channel(); drop(rx);
        service.sender.send(Request::InsertPages(combine_source(&target), combine_source(&donor), 0, canceled.clone(), tx)).unwrap();
        call(&service, |reply| Request::Properties(target.id, target.revision, reply)).unwrap(); assert!(!canceled.exists());
        let output = folder.path().join("unread.pdf"); let _unread_cleanup = UnreadReplyCleanup { service: service.clone(), paths: vec![output.clone()] };
        let retained = unread_document_reply(&service, &output, |reply| Request::InsertPages(combine_source(&target), combine_source(&donor), 2, output.clone(), reply));
        assert!(retained.is_empty(), "Unread insertion result must release only the new session"); assert!(output.exists());
        let reopened = call(&service, |reply| Request::Open(output.clone(), reply)).unwrap(); let _reopened_cleanup = TestDocument { service: service.clone(), id: reopened.id }; assert_eq!(reopened.pages.len(), 12);
        let accepted_path = folder.path().join("accepted.pdf");
        let accepted = tauri::async_runtime::block_on(service.insert_pages_copy(combine_source(&target), combine_source(&donor), 6, accepted_path)).unwrap(); let accepted_id = accepted.document.id; drop(accepted);
        let _accepted_cleanup = TestDocument { service: service.clone(), id: accepted_id };
        assert_eq!(call(&service, |reply| Request::Properties(accepted_id, 0, reply)).unwrap().page_count, 12);
        assert_eq!(std::fs::read(target_path).unwrap(), source); assert_eq!(std::fs::read(donor_path).unwrap(), source);
        call(&service, |reply| Request::Close(target.id, reply)).unwrap(); call(&service, |reply| Request::Close(donor.id, reply)).unwrap();
        assert_eq!(service.print_render_blocking(target_print.token, 0, 64, 64).unwrap().bgra, original_print);
        assert_eq!(service.print_render_blocking(donor_print.token, 0, 64, 64).unwrap().bgra, original_print);
        assert!(call(&service, |reply| Request::Text(accepted_id, 11, 0, reply)).unwrap().contains("Page 6 of 6"));
        assert!(!call(&service, |reply| Request::Render(accepted_id, 11, 64, reply)).unwrap().is_empty());
    }
    #[test]
    fn combine_all_ordered_fixture_pairs_validate_every_output_page_and_preserve_source_history() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let fixtures = [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)];
        for (first_index, (first_fixture, first_count)) in fixtures.iter().enumerate() {
            for (second_index, (second_fixture, second_count)) in fixtures.iter().enumerate() {
                if first_index == second_index { continue; }
                let first_path = root.join(first_fixture); let second_path = root.join(second_fixture);
                let first_bytes = std::fs::read(&first_path).unwrap(); let second_bytes = std::fs::read(&second_path).unwrap();
                let mut first = call(&service, |reply| Request::Open(first_path.clone(), reply)).unwrap();
                let mut second = call(&service, |reply| Request::Open(second_path.clone(), reply)).unwrap();
                for edit in [PageEdit::Move { from: 0, to: 2 }, PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] {
                    first = call(&service, |reply| Request::Edit(first.id, edit, reply)).unwrap();
                }
                for edit in [PageEdit::Delete { pages: vec![1, 3] }, PageEdit::Rotate { pages: vec![0], clockwise: false }, PageEdit::Move { from: 0, to: 1 }] {
                    second = call(&service, |reply| Request::Edit(second.id, edit, reply)).unwrap();
                }
                let path = folder.path().join(format!("pair-{first_count}-{second_count}.pdf"));
                call(&service, |reply| Request::CheckCombine(combine_source(&first), combine_source(&second), reply)).unwrap();
                let started = std::time::Instant::now();
                let combined = call(&service, |reply| Request::Combine(combine_source(&first), combine_source(&second), path.clone(), reply)).unwrap().document;
                println!("combine {first_count}+{second_count}: {} current pages, validation/write {:?}", combined.pages.len(), started.elapsed());
                assert_ne!(combined.id, first.id); assert_ne!(combined.id, second.id);
                assert_eq!(combined.pages.len(), first_count + second_count - 3);
                assert_eq!(combined.revision, 0);
                assert!(!combined.dirty && !combined.can_undo && !combined.can_redo);
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
                for position in 0..combined.pages.len() {
                    let (source, local, original_page, count) = if position < first.pages.len() {
                        let original = match position { 0 => 1, 1 => 0, other => other + 1 };
                        (&first, position, original, first_count)
                    } else {
                        let local = position - first.pages.len();
                        let original = match local { 0 => 2, 1 => 0, other => other + 2 };
                        (&second, local, original, second_count)
                    };
                    let text = call(&service, |reply| Request::Text(combined.id, position as u16, 0, reply)).unwrap();
                    assert!(text.contains(&format!("Page {} of {count}", original_page + 1)), "Output source/order differs at page {position} of {first_count}+{second_count}");
                    assert_eq!((combined.pages[position].width, combined.pages[position].height), (source.pages[local].width, source.pages[local].height));
                    let expected = call(&service, |reply| Request::Render(source.id, local as u16, 64, reply)).unwrap();
                    let actual = call(&service, |reply| Request::Render(combined.id, position as u16, 64, reply)).unwrap();
                    let reopened_actual = call(&service, |reply| Request::Render(reopened.id, position as u16, 64, reply)).unwrap();
                    let decode = |bytes: &[u8]| image::load_from_memory(bytes).unwrap().into_rgba8();
                    assert_eq!(decode(&actual), decode(&expected), "Combined output render differs at page {position}");
                    assert_eq!(decode(&reopened_actual), decode(&expected), "Reopened output render differs at page {position}");
                }
                assert_eq!(std::fs::read(&first_path).unwrap(), first_bytes); assert_eq!(std::fs::read(&second_path).unwrap(), second_bytes);
                for source in [&first, &second] {
                    let properties = call(&service, |reply| Request::Properties(source.id, source.revision, reply)).unwrap();
                    assert_eq!(properties.page_count, source.pages.len());
                    for undo in 0..3 {
                        let restored = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap();
                        assert_eq!(restored.revision, source.revision + undo + 1); assert_eq!(restored.dirty, undo != 2); assert!(restored.can_redo);
                    }
                }
                for id in [first.id, second.id, combined.id, reopened.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
            }
        }
    }
    #[test]
    fn combine_cropped_inherited_original_and_edited_rotations_keep_independent_ink_and_visible_tokens() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        for original in [0, 90, 180, 270] {
            for edited in 0..4 {
                let mut sources = Vec::new(); let mut expected = Vec::new();
                for (index, label) in ["First", "Second"].into_iter().enumerate() {
                    let rotation = if index == 0 { original } else { (360 - original) % 360 };
                    let bytes = crate::text_geometry::tests::fixture(rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 300.0], &format!("{label}OutsideTop\n\n{label}Header\n{label}Body\n\n{label}OutsideBottom"));
                    let mut pdf = lopdf::Document::load_mem(&bytes).unwrap(); let page = pdf.get_pages()[&1];
                    pdf.get_dictionary_mut(page).unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]);
                    let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
                    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] {
                        let value = pdf.get_dictionary_mut(page).unwrap().remove(key).unwrap(); pdf.get_dictionary_mut(parent).unwrap().set(key, value);
                    }
                    let path = folder.path().join(format!("source-{original}-{edited}-{index}.pdf")); pdf.save(&path).unwrap(); let source_bytes = std::fs::read(&path).unwrap();
                    let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
                    for _ in 0..edited { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: index == 0 }, reply)).unwrap(); }
                    let turns = (rotation / 90 + if index == 0 { edited } else { (4 - edited) % 4 }) % 4;
                    let (rect, region, old_width, new_width) = match turns {
                        0 => (CropRect { x: 0.1, y: 70.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 210, 720, 330), 900, 720),
                        1 => (CropRect { x: 80.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (240, 90, 330, 720), 780, 330),
                        2 => (CropRect { x: 0.1, y: 80.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 240, 720, 330), 900, 720),
                        _ => (CropRect { x: 70.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (210, 90, 330, 720), 780, 330),
                    };
                    let before = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, old_width, reply)).unwrap()).unwrap().into_rgb8();
                    expected.push((image::imageops::crop_imm(&before, region.0, region.1, region.2, region.3).to_image(), new_width, turns));
                    info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, rect, reply)).unwrap();
                    sources.push((info, path, source_bytes));
                }
                let path = folder.path().join(format!("combined-{original}-{edited}.pdf"));
                let combined = call(&service, |reply| Request::Combine(combine_source(&sources[0].0), combine_source(&sources[1].0), path.clone(), reply)).unwrap().document;
                let reopened = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
                let output = lopdf::Document::load(&path).unwrap();
                for (index, label) in ["First", "Second"].into_iter().enumerate() {
                    let (expected, width, turns) = &expected[index];
                    let dimensions = if turns % 2 == 0 { (240.0, 110.0) } else { (110.0, 240.0) };
                    assert_eq!((combined.pages[index].width, combined.pages[index].height), dimensions);
                    for id in [combined.id, reopened.id] {
                        let actual = image::load_from_memory(&call(&service, |reply| Request::Render(id, index as u16, *width, reply)).unwrap()).unwrap().into_rgb8(); assert_same_ink(&actual, expected);
                        let text = call(&service, |reply| Request::Text(id, index as u16, 0, reply)).unwrap();
                        let geometry = call(&service, |reply| Request::TextGeometry(id, index as u16, 0, reply)).unwrap(); assert_eq!(geometry.status, "ok");
                        let copied = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
                        for token in [format!("{label}Header"), format!("{label}Body")] { assert!(text.contains(&token), "{original}/{edited}: {text}"); assert!(copied.contains(&token), "{original}/{edited}: {copied}"); }
                        for token in [format!("{label}OutsideTop"), format!("{label}OutsideBottom")] { assert!(!text.contains(&token) && !copied.contains(&token)); }
                    }
                    assert!(String::from_utf8_lossy(&output.get_page_content(output.get_pages()[&(index as u32 + 1)])).contains(&format!("{label}OutsideBottom")), "Crop must retain the underlying hidden content");
                    let (source, source_path, source_bytes) = &sources[index]; assert_eq!(std::fs::read(source_path).unwrap(), *source_bytes);
                    let undo = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(undo.revision, source.revision + 1); assert!(undo.can_redo);
                    call(&service, |reply| Request::Close(source.id, reply)).unwrap();
                }
                for id in [combined.id, reopened.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
            }
        }
    }
    #[test]
    fn combine_duplicate_stale_and_closed_after_preflight_never_write_or_mutate_sources() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll")); let folder = tempfile::tempdir().unwrap();
        let first = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let second = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let path = folder.path().join("must-not-combine.pdf");
        let combine = |first, second| call(&service, |reply| Request::Combine(first, second, path.clone(), reply));
        assert!(combine(combine_source(&first), combine_source(&first)).err().unwrap().contains("different"));
        call(&service, |reply| Request::CheckCombine(combine_source(&first), combine_source(&second), reply)).unwrap();
        let changed = call(&service, |reply| Request::Edit(second.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        assert!(combine(combine_source(&first), combine_source(&second)).err().unwrap().contains("changed")); assert!(!path.exists());
        let restored = call(&service, |reply| Request::Edit(second.id, PageEdit::Undo, reply)).unwrap(); assert!(!restored.dirty && restored.can_redo);
        call(&service, |reply| Request::CheckCombine(combine_source(&first), combine_source(&restored), reply)).unwrap();
        call(&service, |reply| Request::Close(second.id, reply)).unwrap(); assert!(combine(combine_source(&first), combine_source(&restored)).err().unwrap().contains("Second document is closed"));
        call(&service, |reply| Request::Close(first.id, reply)).unwrap(); assert!(combine(combine_source(&first), combine_source(&changed)).err().unwrap().contains("First document is closed"));
        assert!(!path.exists()); assert_eq!(std::fs::read_dir(folder.path()).unwrap().count(), 0);
    }
    #[test]
    fn crop_rotation_inherited_boxes_preview_text_geometry_print_and_reopened_outputs_agree() {
        let _print_lock = print_test_lock();
        use lopdf::Document;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let full = CropRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 };
        for inherited in [false, true] {
            for original_rotation in [0, 90, 180, 270] {
                let bytes = crate::text_geometry::tests::fixture(original_rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 300.0], "OutsideTop\n\nInsideHeader\nInsideBody\n\nOutsideBottom");
                let mut pdf = Document::load_mem(&bytes).unwrap();
                let page = pdf.get_pages()[&1];
                pdf.get_object_mut(page).unwrap().as_dict_mut().unwrap().set("MediaBox", vec![20.into(), 30.into(), 420.into(), 430.into()]);
                if inherited {
                    let parent = pdf.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();
                    for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate"] {
                        let value = pdf.get_object_mut(page).unwrap().as_dict_mut().unwrap().remove(key).unwrap();
                        pdf.get_object_mut(parent).unwrap().as_dict_mut().unwrap().set(key, value);
                    }
                }
                let path = folder.path().join(format!("crop-{inherited}-{original_rotation}.pdf"));
                pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap();
                let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
                let old_snapshot = print_snapshot(&service, info.id, info.revision);
                let original_print = service.print_render_blocking(old_snapshot.token, 0, 900, 900).unwrap();
                for edited in 0..4 {
                    let turns = (original_rotation / 90 + edited) % 4;
                    let before_revision = info.revision;
                    let no_op = call(&service, |reply| Request::Crop(info.id, 0, info.revision, full, reply)).unwrap();
                    assert_eq!(no_op.revision, info.revision); assert_eq!(no_op.dirty, info.dirty); assert_eq!(no_op.can_undo, info.can_undo); assert_eq!(no_op.can_redo, info.can_redo);
                    // These fixed pixel regions describe [80,150,320,260] inside the fixture's [50,70,350,330] visible box.
                    let (rect, expected_region, old_width, new_width) = match turns {
                        0 => (CropRect { x: 0.1, y: 70.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 210, 720, 330), 900, 720),
                        1 => (CropRect { x: 80.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (240, 90, 330, 720), 780, 330),
                        2 => (CropRect { x: 0.1, y: 80.0 / 260.0, width: 0.8, height: 110.0 / 260.0 }, (90, 240, 720, 330), 900, 720),
                        _ => (CropRect { x: 70.0 / 260.0, y: 0.1, width: 110.0 / 260.0, height: 0.8 }, (210, 90, 330, 720), 780, 330),
                    };
                    let before = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, old_width, reply)).unwrap()).unwrap().into_rgb8();
                    let expected = image::imageops::crop_imm(&before, expected_region.0, expected_region.1, expected_region.2, expected_region.3).to_image();
                    info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, rect, reply)).unwrap();
                    assert_eq!(info.revision, before_revision + 1); assert!(info.dirty && info.can_undo);
                    let (expected_width, expected_height) = if turns % 2 == 0 { (240.0, 110.0) } else { (110.0, 240.0) };
                    assert!((info.pages[0].width - expected_width).abs() < 0.001 && (info.pages[0].height - expected_height).abs() < 0.001);
                    assert!(call(&service, |reply| Request::Crop(info.id, 0, before_revision, rect, reply)).err().unwrap().contains("changed"));
                    assert!(call(&service, |reply| Request::Text(info.id, 0, before_revision, reply)).is_err());
                    assert!(call(&service, |reply| Request::TextGeometry(info.id, 0, before_revision, reply)).is_err());
                    let no_op = call(&service, |reply| Request::Crop(info.id, 0, info.revision, full, reply)).unwrap(); assert_eq!(no_op.revision, info.revision);
                    let preview = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, new_width, reply)).unwrap()).unwrap().into_rgb8();
                    assert_same_ink(&preview, &expected);
                    let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap();
                    assert_eq!(geometry.status, "ok", "{:?}", geometry.reason);
                    let positioned = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
                    let text = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap();
                    for words in [&positioned, &text] {
                        for word in ["InsideHeader", "InsideBody"] { assert!(words.contains(word), "Lost {word}: original {original_rotation}, edit {edited}, inherited {inherited}: {words}"); }
                        for word in ["OutsideTop", "OutsideBottom"] { assert!(!words.contains(word), "Copied hidden {word}"); }
                    }
                    let boxes = geometry.characters.iter().filter_map(|character| character.bounds.as_ref()).collect::<Vec<_>>();
                    for (x, y, pixel) in preview.enumerate_pixels().filter(|(_, _, pixel)| pixel.0.iter().any(|value| *value < 100)) {
                        let _ = pixel;
                        assert!(boxes.iter().any(|bounds| x as f32 >= bounds.x * preview.width() as f32 - 2.0 && x as f32 <= (bounds.x + bounds.width) * preview.width() as f32 + 2.0 && y as f32 >= bounds.y * preview.height() as f32 - 2.0 && y as f32 <= (bounds.y + bounds.height) * preview.height() as f32 + 2.0), "Crop geometry missed rendered ink");
                    }
                    let snapshot = print_snapshot(&service, info.id, info.revision);
                    assert!(service.print_render_blocking(snapshot.token, 0, 0, 900).is_err());
                    let printed = service.print_render_blocking(snapshot.token, 0, new_width as u32, expected_region.3).unwrap();
                    let print_rgb = image::RgbImage::from_raw(printed.width, printed.height, printed.bgra.chunks_exact(4).flat_map(|pixel| [pixel[2], pixel[1], pixel[0]]).collect()).unwrap();
                    assert_same_ink(&print_rgb, &expected);
                    let prior_print = service.print_render_blocking(old_snapshot.token, 0, 900, 900).unwrap();
                    assert_eq!(prior_print.bgra, original_print.bgra, "Crop or failure changed an earlier print snapshot");
                    let copy = folder.path().join(format!("copy-{inherited}-{original_rotation}-{edited}.pdf"));
                    let saved = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap();
                    assert!(saved.document.dirty); assert_eq!(saved.document.revision, info.revision);
                    let output = Document::load(&copy).unwrap();
                    assert_eq!(output.get_page_content(output.get_pages()[&1]), pdf.get_page_content(page));
                    assert!(output.extract_text(&[1]).unwrap().contains("OutsideBottom"), "Crop must hide content without deleting it");
                    let reopened = call(&service, |reply| Request::Open(copy.clone(), reply)).unwrap();
                    let reopened_image = image::load_from_memory(&call(&service, |reply| Request::Render(reopened.id, 0, new_width, reply)).unwrap()).unwrap().into_rgb8();
                    assert_same_ink(&reopened_image, &expected);
                    let reopened_text = call(&service, |reply| Request::Text(reopened.id, 0, 0, reply)).unwrap(); assert!(reopened_text.contains("InsideHeader") && !reopened_text.contains("OutsideBottom"));
                    call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
                    let split_folder = folder.path().join(format!("split-{inherited}-{original_rotation}-{edited}"));
                    let split = call(&service, |reply| Request::Split(info.id, info.revision, 1, split_folder, reply)).unwrap(); assert_eq!(split.files.len(), 1);
                    let split_info = call(&service, |reply| Request::Open(PathBuf::from(&split.files[0].path), reply)).unwrap();
                    let split_image = image::load_from_memory(&call(&service, |reply| Request::Render(split_info.id, 0, new_width, reply)).unwrap()).unwrap().into_rgb8(); assert_same_ink(&split_image, &expected);
                    call(&service, |reply| Request::Close(split_info.id, reply)).unwrap();
                    info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
                    let restored = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, old_width, reply)).unwrap()).unwrap().into_rgb8(); assert_eq!(restored, before);
                    info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap();
                    let redone = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 0, new_width, reply)).unwrap()).unwrap().into_rgb8(); assert_same_ink(&redone, &expected);
                    info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
                    let held = service.print_render_blocking(snapshot.token, 0, new_width as u32, expected_region.3).unwrap(); assert_eq!(held.bgra, printed.bgra, "Crop print snapshot changed after undo");
                    call(&service, |reply| Request::EndPrint(snapshot.token, reply)).unwrap();
                    assert_eq!(std::fs::read(&path).unwrap(), source);
                    if edited < 3 { info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap(); }
                }
                call(&service, |reply| Request::Close(info.id, reply)).unwrap();
                assert!(call(&service, |reply| Request::Crop(info.id, 0, info.revision, full, reply)).err().unwrap().contains("closed"));
                assert_eq!(service.print_render_blocking(old_snapshot.token, 0, 900, 900).unwrap().bgra, original_print.bgra);
                call(&service, |reply| Request::EndPrint(old_snapshot.token, reply)).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), source);
                let original = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
                assert_eq!(original.pages[0].width, if original_rotation % 180 == 0 { 300.0 } else { 260.0 });
                call(&service, |reply| Request::Close(original.id, reply)).unwrap();
            }
        }
    }
    #[test]
    fn crop_current_edited_page_all_fixtures_and_repeated_crop_preserve_other_pages_and_source() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap();
            let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 2 }, reply)).unwrap();
            let original_text = call(&service, |reply| Request::Text(info.id, 2, info.revision, reply)).unwrap();
            assert!(original_text.contains(&format!("Page 1 of {count}")));
            let other_before = call(&service, |reply| Request::Render(info.id, 0, 201, reply)).unwrap();
            let original_crop_page = call(&service, |reply| Request::Render(info.id, 2, 200, reply)).unwrap();
            let started = std::time::Instant::now();
            info = call(&service, |reply| Request::Crop(info.id, 2, info.revision, CropRect { x: 0.0, y: 0.0, width: 1.0, height: 0.8 }, reply)).unwrap();
            assert_eq!(info.pages.len(), count); assert!(info.dirty && info.can_undo);
            assert!(call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap().contains(&format!("Page 2 of {count}")));
            assert!(!call(&service, |reply| Request::Text(info.id, 2, info.revision, reply)).unwrap().contains(&format!("Page 1 of {count}")), "Footer should be outside the crop in {fixture}");
            let geometry = call(&service, |reply| Request::TextGeometry(info.id, 2, info.revision, reply)).unwrap();
            assert_eq!(geometry.status, "ok", "{fixture}: {:?}", geometry.reason);
            assert!(!geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>().contains(&format!("Page 1 of {count}")));
            let first_size = info.pages[2].clone();
            info = call(&service, |reply| Request::Crop(info.id, 2, info.revision, CropRect { x: 0.1, y: 0.1, width: 0.8, height: 0.8 }, reply)).unwrap();
            assert!((info.pages[2].width - first_size.width * 0.8).abs() < 0.001 && (info.pages[2].height - first_size.height * 0.8).abs() < 0.001);
            let plan_revision = info.revision;
            for rect in [
                CropRect { x: f64::NAN, y: 0.0, width: 0.5, height: 0.5 },
                CropRect { x: 0.0, y: 0.0, width: f64::INFINITY, height: 0.5 },
                CropRect { x: -0.1, y: 0.0, width: 0.5, height: 0.5 },
                CropRect { x: 0.0, y: 0.0, width: 0.0, height: 0.5 },
                CropRect { x: 0.2, y: 0.0, width: 0.9, height: 0.5 },
                CropRect { x: 0.0, y: 0.0, width: 0.0001, height: 0.5 },
            ] { assert!(call(&service, |reply| Request::Crop(info.id, 2, info.revision, rect, reply)).is_err()); }
            assert!(call(&service, |reply| Request::Crop(info.id, count as u16, info.revision, CropRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }, reply)).is_err());
            assert_eq!(call(&service, |reply| Request::Properties(info.id, plan_revision, reply)).unwrap().page_count, count);
            assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 201, reply)).unwrap(), other_before, "Crop changed an unrelated current page");
            let preview = image::load_from_memory(&call(&service, |reply| Request::Render(info.id, 2, 200, reply)).unwrap()).unwrap().into_rgb8();
            let copy = folder.path().join(format!("fixture-{count}.pdf"));
            let saved = call(&service, |reply| Request::Save(info.id, None, copy.clone(), reply)).unwrap(); assert!(saved.document.dirty);
            let reopened = call(&service, |reply| Request::Open(copy, reply)).unwrap();
            assert_eq!(reopened.pages.len(), count);
            let after = image::load_from_memory(&call(&service, |reply| Request::Render(reopened.id, 2, 200, reply)).unwrap()).unwrap().into_rgb8();
            assert_eq!(after, preview);
            call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
            println!("crop fixture={fixture} pages={count} repeated-crop-export-reopen-ms={:.1}", started.elapsed().as_secs_f64() * 1000.0);
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
            assert!((info.pages[2].width - first_size.width).abs() < 0.001 && (info.pages[2].height - first_size.height).abs() < 0.001);
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
            assert_eq!(call(&service, |reply| Request::Render(info.id, 2, 200, reply)).unwrap(), original_crop_page);
            assert_eq!(std::fs::read(&path).unwrap(), source);
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
    }
    #[test]
    fn batch_crop_mixed_current_bounds_is_atomic_and_one_undo_entry() {
        let _print_lock = print_test_lock();
        use lopdf::{dictionary, Document};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let mut pdf = Document::load(root.join("resources/welcome.pdf")).unwrap();
        let pages: Vec<_> = pdf.get_pages().values().copied().collect();
        pdf.get_object_mut(pages[1]).unwrap().as_dict_mut().unwrap().set("MediaBox", vec![0.into(), 0.into(), 400.into(), 300.into()]);
        pdf.get_object_mut(pages[1]).unwrap().as_dict_mut().unwrap().set("CropBox", vec![10.into(), 20.into(), 390.into(), 280.into()]);
        pdf.get_object_mut(pages[2]).unwrap().as_dict_mut().unwrap().set("Rotate", 90);
        let path = folder.path().join("batch-mixed.pdf"); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap();
        let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.05, y: 0.1, width: 0.9, height: 0.8 }, reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![1, 2], clockwise: true }, reply)).unwrap();
        let before_revision = info.revision; let before_sizes = info.pages.clone();
        let before_first = call(&service, |reply| Request::Render(info.id, 0, 173, reply)).unwrap();
        let before_unrelated = call(&service, |reply| Request::Render(info.id, 4, 173, reply)).unwrap();
        let old_snapshot = print_snapshot(&service, info.id, info.revision);
        let old_print = service.print_render_blocking(old_snapshot.token, 0, 300, 300).unwrap();
        let insets = CropInsets { top: 10.0, right: 20.0, bottom: 30.0, left: 40.0 };

        for targets in [vec![], vec![1, 0], vec![0, 0], vec![0, 2, 6]] {
            assert!(call(&service, |reply| Request::CropPages(info.id, targets, info.revision, insets, reply)).is_err());
            let unchanged = call(&service, |reply| Request::Properties(info.id, info.revision, reply)).unwrap(); assert_eq!(unchanged.page_count, 6);
            assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 173, reply)).unwrap(), before_first, "An invalid last target must not crop the first target");
            assert_eq!(call(&service, |reply| Request::Render(info.id, 4, 173, reply)).unwrap(), before_unrelated);
        }
        for invalid in [
            CropInsets { top: f64::NAN, ..insets }, CropInsets { left: f64::INFINITY, ..insets },
            CropInsets { right: -1.0, ..insets }, CropInsets { top: 0.0, right: 0.0, bottom: 0.0, left: 0.0 },
            CropInsets { top: 0.0, right: 0.0, bottom: 0.0, left: f64::from(before_sizes[1].width) },
        ] { assert!(call(&service, |reply| Request::CropPages(info.id, vec![0, 1], info.revision, invalid, reply)).is_err()); }
        let (reply, canceled) = oneshot::channel(); drop(canceled); service.sender.send(Request::CropPages(info.id, vec![0, 1, 2], info.revision, insets, reply)).unwrap();
        assert_eq!(call(&service, |reply| Request::Properties(info.id, info.revision, reply)).unwrap().page_count, 6);

        info = call(&service, |reply| Request::CropPages(info.id, vec![0, 1, 2], info.revision, insets, reply)).unwrap();
        assert_eq!(info.revision, before_revision + 1); assert!(info.can_undo && !info.can_redo);
        assert!(call(&service, |reply| Request::CropPages(info.id, vec![0, 1, 2], before_revision, insets, reply)).err().unwrap().contains("changed"));
        for index in 0..3 {
            assert!((info.pages[index].width - (before_sizes[index].width - 60.0)).abs() < 0.01, "width {index}");
            assert!((info.pages[index].height - (before_sizes[index].height - 40.0)).abs() < 0.01, "height {index}");
        }
        assert_eq!(call(&service, |reply| Request::Render(info.id, 4, 173, reply)).unwrap(), before_unrelated);
        let cropped_sizes = info.pages.clone();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap(); assert_eq!(info.revision, before_revision + 2);
        for index in 0..3 { assert!((info.pages[index].width - before_sizes[index].width).abs() < 0.01 && (info.pages[index].height - before_sizes[index].height).abs() < 0.01); }
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap(); assert_eq!(info.revision, before_revision + 3);
        for index in 0..3 { assert!((info.pages[index].width - cropped_sizes[index].width).abs() < 0.01 && (info.pages[index].height - cropped_sizes[index].height).abs() < 0.01); }
        let current_snapshot = print_snapshot(&service, info.id, info.revision);
        let current_print = service.print_render_blocking(current_snapshot.token, 0, 300, 300).unwrap(); assert_ne!(current_print.bgra, old_print.bgra);
        for index in 1..3 { let bitmap = service.print_render_blocking(current_snapshot.token, index, 300, 300).unwrap(); assert!(!bitmap.bgra.is_empty()); }
        assert_eq!(service.print_render_blocking(old_snapshot.token, 0, 300, 300).unwrap().bgra, old_print.bgra);
        let output = folder.path().join("batch-mixed-output.pdf");
        call(&service, |reply| Request::Save(info.id, None, output.clone(), reply)).unwrap();
        let reopened = call(&service, |reply| Request::Open(output, reply)).unwrap();
        for index in 0..3 { assert!((reopened.pages[index].width - cropped_sizes[index].width).abs() < 0.01 && (reopened.pages[index].height - cropped_sizes[index].height).abs() < 0.01); }
        let reopened_snapshot = print_snapshot(&service, reopened.id, reopened.revision);
        assert_eq!(service.print_render_blocking(reopened_snapshot.token, 0, 300, 300).unwrap().bgra, current_print.bgra);
        assert_eq!(std::fs::read(path).unwrap(), source);
        call(&service, |reply| Request::Close(reopened.id, reply)).unwrap(); let closed_id = info.id; let closed_revision = info.revision; call(&service, |reply| Request::Close(closed_id, reply)).unwrap();
        assert!(call(&service, |reply| Request::CropPages(closed_id, vec![0], closed_revision, insets, reply)).err().unwrap().contains("closed"));

        let mut signed = Document::load(root.join("resources/welcome.pdf")).unwrap(); signed.catalog_mut().unwrap().set("Perms", dictionary! {});
        let signed_path = folder.path().join("batch-signed.pdf"); signed.save(&signed_path).unwrap(); let signed_source = std::fs::read(&signed_path).unwrap();
        let signed_info = call(&service, |reply| Request::Open(signed_path.clone(), reply)).unwrap(); let signed_before = call(&service, |reply| Request::Render(signed_info.id, 0, 173, reply)).unwrap();
        assert!(call(&service, |reply| Request::CropPages(signed_info.id, vec![0, 1], signed_info.revision, insets, reply)).err().unwrap().contains("Signed or certified"));
        assert_eq!(call(&service, |reply| Request::Render(signed_info.id, 0, 173, reply)).unwrap(), signed_before); assert_eq!(std::fs::read(signed_path).unwrap(), signed_source);
        call(&service, |reply| Request::Close(signed_info.id, reply)).unwrap();
    }
    #[test]
    fn reset_crops_restores_source_view_and_annotations_atomically_and_preserves_outputs() {
        let _print_lock = print_test_lock();
        let _password_lock = password_test_lock();
        use lopdf::{dictionary, Document};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();

        let mut pdf = Document::load(root.join("resources/welcome.pdf")).unwrap();
        let first = pdf.get_pages()[&1];
        pdf.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("CropBox", vec![36.into(), 72.into(), 576.into(), 720.into()]);
        pdf.get_object_mut(first).unwrap().as_dict_mut().unwrap().set("Rotate", 90);
        let path = folder.path().join("reset-source.pdf"); pdf.save(&path).unwrap();
        let source = std::fs::read(&path).unwrap();
        let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::Create(0, CropRect { x: 0.01, y: 0.01, width: 0.08, height: 0.08 }, "Reset crop note".into()), reply)).unwrap();
        let restored_sizes = info.pages.clone();
        let restored_render = call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap();
        let restored_annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap();
        assert_eq!(restored_annotations.annotations.len(), 1); assert!(restored_annotations.annotations[0].rect.is_some());
        let restored_snapshot = print_snapshot(&service, info.id, info.revision);
        let restored_print = service.print_render_blocking(restored_snapshot.token, 0, 300, 300).unwrap();

        info = call(&service, |reply| Request::CropPages(info.id, vec![0, 1], info.revision, CropInsets { top: 72.0, right: 72.0, bottom: 72.0, left: 72.0 }, reply)).unwrap();
        let cropped_revision = info.revision; let cropped_sizes = info.pages.clone();
        assert!(call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap().annotations[0].rect.is_none());
        let cropped_snapshot = print_snapshot(&service, info.id, info.revision);
        let cropped_print = service.print_render_blocking(cropped_snapshot.token, 0, 300, 300).unwrap();
        assert_ne!(cropped_print.bgra, restored_print.bgra);

        let stable_render = call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap();
        for pages in [vec![], vec![1, 0], vec![0, 0], vec![0, 6]] {
            assert!(call(&service, |reply| Request::ResetCrops(info.id, pages, info.revision, reply)).is_err());
            assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap(), stable_render);
        }
        let (reply, receiver) = oneshot::channel(); drop(receiver);
        service.sender.send(Request::ResetCrops(info.id, vec![0, 2], info.revision, reply)).unwrap();
        assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap(), stable_render, "A closed reply must not reset crops");

        info = call(&service, |reply| Request::ResetCrops(info.id, vec![0, 2], info.revision, reply)).unwrap();
        assert_eq!(info.revision, cropped_revision + 1); assert!(info.can_undo && !info.can_redo);
        assert_eq!((info.pages[0].width, info.pages[0].height), (restored_sizes[0].width, restored_sizes[0].height));
        assert_eq!((info.pages[1].width, info.pages[1].height), (cropped_sizes[1].width, cropped_sizes[1].height), "Unselected crop must remain");
        assert_eq!((info.pages[2].width, info.pages[2].height), (restored_sizes[2].width, restored_sizes[2].height), "Selected uncropped page must stay unchanged");
        assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap(), restored_render);
        let reset_annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap();
        assert_eq!(reset_annotations.annotations[0].id, restored_annotations.annotations[0].id);
        assert_eq!(reset_annotations.annotations[0].kind, restored_annotations.annotations[0].kind);
        assert_eq!(reset_annotations.annotations[0].contents.as_deref(), Some("Reset crop note"));
        let reset_rect = reset_annotations.annotations[0].rect.as_ref().unwrap(); let restored_rect = restored_annotations.annotations[0].rect.as_ref().unwrap();
        assert!((reset_rect.x - restored_rect.x).abs() < 1e-9 && (reset_rect.y - restored_rect.y).abs() < 1e-9 && (reset_rect.width - restored_rect.width).abs() < 1e-9 && (reset_rect.height - restored_rect.height).abs() < 1e-9);
        let reset_snapshot = print_snapshot(&service, info.id, info.revision);
        assert_eq!(service.print_render_blocking(reset_snapshot.token, 0, 300, 300).unwrap().bgra, restored_print.bgra);
        assert_eq!(service.print_render_blocking(cropped_snapshot.token, 0, 300, 300).unwrap().bgra, cropped_print.bgra, "Reset must not mutate an existing print snapshot");
        assert_eq!(service.print_render_blocking(restored_snapshot.token, 0, 300, 300).unwrap().bgra, restored_print.bgra);
        assert!(call(&service, |reply| Request::ResetCrops(info.id, vec![0], cropped_revision, reply)).err().unwrap().contains("changed"));

        let no_op_revision = info.revision; let no_op_dirty = info.dirty; let no_op_undo = info.can_undo; let no_op_redo = info.can_redo;
        let no_op = call(&service, |reply| Request::ResetCrops(info.id, vec![0, 2], info.revision, reply)).unwrap();
        assert_eq!((no_op.revision, no_op.dirty, no_op.can_undo, no_op.can_redo), (no_op_revision, no_op_dirty, no_op_undo, no_op_redo));
        assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 240, reply)).unwrap(), restored_render);

        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
        assert_eq!((info.pages[0].width, info.pages[0].height), (cropped_sizes[0].width, cropped_sizes[0].height));
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap();
        assert_eq!((info.pages[0].width, info.pages[0].height), (restored_sizes[0].width, restored_sizes[0].height));

        let output = folder.path().join("reset-output.pdf");
        let saved = call(&service, |reply| Request::Save(info.id, None, output.clone(), reply)).unwrap(); assert!(saved.document.dirty);
        let saved_pdf = Document::load(&output).unwrap(); let saved_page = saved_pdf.get_pages()[&1];
        let raw_box = |key: &[u8]| saved_pdf.get_dictionary(saved_page).unwrap().get(key).unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<_>>();
        assert_eq!(raw_box(b"MediaBox"), vec![0.0, 0.0, 612.0, 792.0]);
        assert_eq!(raw_box(b"CropBox"), vec![36.0, 72.0, 576.0, 720.0], "Reset output must retain the exact direct source CropBox object");
        let proof = root.join("../target/reset-crop-probe"); std::fs::create_dir_all(&proof).unwrap();
        let proof_source = proof.join("source-direct-crop-rotation.pdf"); let proof_output = proof.join("reset-output.pdf");
        std::fs::write(&proof_source, &source).unwrap(); std::fs::copy(&output, &proof_output).unwrap();
        println!("RESET_CROP_PROBE source={} output={}", proof_source.display(), proof_output.display());
        let reopened = call(&service, |reply| Request::Open(output, reply)).unwrap();
        assert_eq!((reopened.pages[0].width, reopened.pages[0].height), (restored_sizes[0].width, restored_sizes[0].height));
        assert_eq!(call(&service, |reply| Request::Render(reopened.id, 0, 240, reply)).unwrap(), restored_render);
        let reopened_annotations = call(&service, |reply| Request::Annotations(reopened.id, reopened.revision, reply)).unwrap();
        assert_eq!(reopened_annotations.annotations.len(), 1); assert_eq!(reopened_annotations.annotations[0].contents.as_deref(), Some("Reset crop note")); assert!(reopened_annotations.annotations[0].rect.is_some());
        let reopened_snapshot = print_snapshot(&service, reopened.id, reopened.revision);
        assert_eq!(service.print_render_blocking(reopened_snapshot.token, 0, 300, 300).unwrap().bgra, restored_print.bgra);
        assert_eq!(std::fs::read(&path).unwrap(), source);
        call(&service, |reply| Request::Close(reopened.id, reply)).unwrap();
        let closed_id = info.id; let closed_revision = info.revision; call(&service, |reply| Request::Close(closed_id, reply)).unwrap();
        assert!(call(&service, |reply| Request::ResetCrops(closed_id, vec![0], closed_revision, reply)).err().unwrap().contains("closed"));

        let label_path = root.join("tests/fixtures/reportlab-page-labels.pdf"); let label_source = std::fs::read(&label_path).unwrap();
        let labels = call(&service, |reply| Request::Open(label_path.clone(), reply)).unwrap();
        let before_labels = call(&service, |reply| Request::PageLabels(labels.id, labels.revision, reply)).unwrap();
        let cropped = call(&service, |reply| Request::CropPages(labels.id, vec![0], labels.revision, CropInsets { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 }, reply)).unwrap();
        let reset = call(&service, |reply| Request::ResetCrops(cropped.id, vec![0], cropped.revision, reply)).unwrap();
        let after_labels = call(&service, |reply| Request::PageLabels(reset.id, reset.revision, reply)).unwrap();
        assert_eq!(after_labels.labels, before_labels.labels); assert_eq!(std::fs::read(label_path).unwrap(), label_source);

        for kind in ["certified", "signature"] {
            let mut protected = Document::load(root.join("resources/welcome.pdf")).unwrap();
            if kind == "certified" { protected.catalog_mut().unwrap().set("Perms", dictionary! {}); } else { protected.add_object(dictionary! { "Type" => "Sig" }); }
            let protected_path = folder.path().join(format!("reset-{kind}.pdf")); protected.save(&protected_path).unwrap(); let protected_source = std::fs::read(&protected_path).unwrap();
            let protected_info = call(&service, |reply| Request::Open(protected_path.clone(), reply)).unwrap(); let before = call(&service, |reply| Request::Render(protected_info.id, 0, 173, reply)).unwrap();
            assert!(call(&service, |reply| Request::ResetCrops(protected_info.id, vec![0], protected_info.revision, reply)).err().unwrap().contains("Signed or certified"));
            assert_eq!(call(&service, |reply| Request::Render(protected_info.id, 0, 173, reply)).unwrap(), before); assert_eq!(std::fs::read(protected_path).unwrap(), protected_source);
        }
        for (kind, user_password, permissions) in [("encrypted", "reset password", lopdf::Permissions::all()), ("restricted", "", lopdf::Permissions::empty())] {
            let mut protected = Document::load(root.join("resources/welcome.pdf")).unwrap();
            protected.trailer.set("ID", vec![lopdf::Object::string_literal(format!("reset-{kind}")), lopdf::Object::string_literal(format!("reset-{kind}"))]);
            let encryption = lopdf::EncryptionVersion::V2 { document: &protected, owner_password: "owner password", user_password, key_length: 128, permissions };
            protected.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
            let protected_path = folder.path().join(format!("reset-{kind}.pdf")); protected.save(&protected_path).unwrap(); let protected_source = std::fs::read(&protected_path).unwrap();
            let opened = call(&service, |reply| Request::BeginOpen(protected_path.clone(), reply)).unwrap();
            let protected_info = match opened {
                OpenResult::Opened { document, .. } => document,
                OpenResult::PasswordRequired { request_id, .. } => match call(&service, |reply| Request::Unlock(request_id, user_password.to_owned(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{kind} did not unlock") },
            };
            let before = call(&service, |reply| Request::Render(protected_info.id, 0, 173, reply)).unwrap();
            assert!(call(&service, |reply| Request::ResetCrops(protected_info.id, vec![0], protected_info.revision, reply)).is_err(), "{kind}");
            assert_eq!(call(&service, |reply| Request::Render(protected_info.id, 0, 173, reply)).unwrap(), before); assert_eq!(std::fs::read(protected_path).unwrap(), protected_source);
        }
    }
    #[test]
    fn reset_crops_restore_first_middle_last_pages_on_representative_fixtures() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap();
            let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap(); assert_eq!(info.pages.len(), count);
            let targets = vec![0u16, (count / 2) as u16, (count - 1) as u16];
            let original_sizes = targets.iter().map(|&page| info.pages[page as usize].clone()).collect::<Vec<_>>();
            let original_renders = targets.iter().map(|&page| call(&service, |reply| Request::Render(info.id, page, 96, reply)).unwrap()).collect::<Vec<_>>();
            let unrelated = call(&service, |reply| Request::Render(info.id, 1, 97, reply)).unwrap();
            let cropped = call(&service, |reply| Request::CropPages(info.id, targets.clone(), info.revision, CropInsets { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 }, reply)).unwrap();
            for (position, &page) in targets.iter().enumerate() {
                assert!((cropped.pages[page as usize].width - (original_sizes[position].width - 6.0)).abs() < 0.01);
                assert!((cropped.pages[page as usize].height - (original_sizes[position].height - 4.0)).abs() < 0.01);
            }
            let reset = call(&service, |reply| Request::ResetCrops(info.id, targets.clone(), cropped.revision, reply)).unwrap();
            assert_eq!(reset.revision, cropped.revision + 1); assert!(reset.can_undo && !reset.can_redo);
            for (position, &page) in targets.iter().enumerate() {
                assert_eq!((reset.pages[page as usize].width, reset.pages[page as usize].height), (original_sizes[position].width, original_sizes[position].height), "{fixture} page {page}");
                assert_eq!(call(&service, |reply| Request::Render(info.id, page, 96, reply)).unwrap(), original_renders[position], "{fixture} page {page}");
            }
            assert_eq!(call(&service, |reply| Request::Render(info.id, 1, 97, reply)).unwrap(), unrelated, "{fixture} unrelated page");
            assert_eq!(std::fs::read(&path).unwrap(), source); call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
    }
    #[test]
    fn batch_crop_minimum_boundary_labels_and_all_corpora_preserve_source_export_and_print() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let label_path = root.join("tests/fixtures/reportlab-page-labels.pdf");
        let label_source = std::fs::read(&label_path).unwrap();
        let labels = call(&service, |reply| Request::Open(label_path.clone(), reply)).unwrap();
        let before_labels = call(&service, |reply| Request::PageLabels(labels.id, labels.revision, reply)).unwrap();
        let one_point = CropInsets { top: 0.0, right: 0.0, bottom: 0.0, left: f64::from(labels.pages[0].width) - 1.0 };
        let labels = call(&service, |reply| Request::CropPages(labels.id, vec![0], labels.revision, one_point, reply)).unwrap();
        assert!((labels.pages[0].width - 1.0).abs() < 0.001);
        let after_labels = call(&service, |reply| Request::PageLabels(labels.id, labels.revision, reply)).unwrap();
        assert_eq!(after_labels.labels, before_labels.labels); assert_eq!(std::fs::read(&label_path).unwrap(), label_source);
        assert!(call(&service, |reply| Request::Edit(labels.id, PageEdit::Move { from: 0, to: 1 }, reply)).err().unwrap().contains("page labels"));
        call(&service, |reply| Request::Close(labels.id, reply)).unwrap();

        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let path = root.join(fixture); let source = std::fs::read(&path).unwrap();
            let mut info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            assert_eq!(info.pages.len(), count);
            let targets = vec![0u16, (count / 2) as u16, (count - 1) as u16];
            let before_sizes = info.pages.clone(); let unrelated = call(&service, |reply| Request::Render(info.id, 1, 167, reply)).unwrap();
            let old_snapshot = print_snapshot(&service, info.id, info.revision);
            let old_print = service.print_render_blocking(old_snapshot.token, count / 2, 240, 240).unwrap();
            info = call(&service, |reply| Request::CropPages(info.id, targets.clone(), info.revision, CropInsets { top: 1.0, right: 2.0, bottom: 3.0, left: 4.0 }, reply)).unwrap();
            assert_eq!(info.revision, 1); assert!(info.can_undo);
            for &target in &targets { let index = target as usize; assert!((info.pages[index].width - (before_sizes[index].width - 6.0)).abs() < 0.01); assert!((info.pages[index].height - (before_sizes[index].height - 4.0)).abs() < 0.01); }
            assert_eq!(call(&service, |reply| Request::Render(info.id, 1, 167, reply)).unwrap(), unrelated);
            let current_snapshot = print_snapshot(&service, info.id, info.revision);
            let current_print = service.print_render_blocking(current_snapshot.token, 0, 240, 240).unwrap();
            for &target in &targets[1..] { assert!(!service.print_render_blocking(current_snapshot.token, target as usize, 240, 240).unwrap().bgra.is_empty()); }
            assert_eq!(service.print_render_blocking(old_snapshot.token, count / 2, 240, 240).unwrap().bgra, old_print.bgra);
            let cropped_sizes = info.pages.clone();
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
            for &target in &targets { let index = target as usize; assert!((info.pages[index].width - before_sizes[index].width).abs() < 0.01 && (info.pages[index].height - before_sizes[index].height).abs() < 0.01); }
            info = call(&service, |reply| Request::Edit(info.id, PageEdit::Redo, reply)).unwrap();
            let output = folder.path().join(format!("batch-corpus-{count}.pdf")); call(&service, |reply| Request::Save(info.id, None, output.clone(), reply)).unwrap();
            let reopened = call(&service, |reply| Request::Open(output, reply)).unwrap();
            for &target in &targets { let index = target as usize; assert!((reopened.pages[index].width - cropped_sizes[index].width).abs() < 0.01 && (reopened.pages[index].height - cropped_sizes[index].height).abs() < 0.01); assert_eq!(call(&service, |reply| Request::Render(reopened.id, target, 181, reply)).unwrap(), call(&service, |reply| Request::Render(info.id, target, 181, reply)).unwrap()); }
            let reopened_snapshot = print_snapshot(&service, reopened.id, reopened.revision); assert_eq!(service.print_render_blocking(reopened_snapshot.token, 0, 240, 240).unwrap().bgra, current_print.bgra);
            assert_eq!(std::fs::read(&path).unwrap(), source);
            call(&service, |reply| Request::Close(reopened.id, reply)).unwrap(); call(&service, |reply| Request::Close(info.id, reply)).unwrap();
            println!("batch crop fixture={fixture} pages={count} targets={:?}", targets);
        }
    }
    #[test]
    fn crop_minimum_point_and_edge_rounding_and_protected_failures_restore_source_page() {
        let _print_lock = print_test_lock();
        use lopdf::{dictionary, Document};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        for kind in ["ordinary", "certified", "signature", "byte_range"] {
            let mut pdf = Document::load(root.join("resources/welcome.pdf")).unwrap();
            match kind {
                "certified" => { pdf.catalog_mut().unwrap().set("Perms", dictionary! {}); }
                "signature" => { pdf.add_object(dictionary! { "Type" => "Sig" }); }
                "byte_range" => { pdf.add_object(dictionary! { "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] }); }
                _ => {}
            }
            let path = folder.path().join(format!("protected-{kind}.pdf")); pdf.save(&path).unwrap(); let source = std::fs::read(&path).unwrap();
            let info = call(&service, |reply| Request::Open(path.clone(), reply)).unwrap();
            let snapshot = print_snapshot(&service, info.id, 0);
            let before = service.print_render_blocking(snapshot.token, 0, 300, 400).unwrap();
            if kind == "ordinary" {
                let width = f64::from(info.pages[0].width);
                let changed = call(&service, |reply| Request::Crop(info.id, 0, 0, CropRect { x: (width - 1.0) / width, y: 0.0, width: 1.0 / width + 1e-15, height: 1.0 }, reply)).unwrap();
                assert_eq!(changed.pages[0].width, 1.0); assert_eq!(changed.revision, 1);
                call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
            } else {
                for rect in [CropRect { x: 0.1, y: 0.1, width: 0.8, height: 0.8 }, CropRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }] {
                    assert!(call(&service, |reply| Request::Crop(info.id, 0, 0, rect, reply)).err().unwrap().contains("Signed or certified"));
                }
                let properties = call(&service, |reply| Request::Properties(info.id, 0, reply)).unwrap(); assert_eq!(properties.page_count, 6);
            }
            assert_eq!(service.print_render_blocking(snapshot.token, 0, 300, 400).unwrap().bgra, before.bgra, "Crop or rejected operation changed source boundaries");
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
            assert_eq!(service.print_render_blocking(snapshot.token, 0, 300, 400).unwrap().bgra, before.bgra);
            call(&service, |reply| Request::EndPrint(snapshot.token, reply)).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), source);
        }
    }
    #[test]
    fn page_text_and_geometry_keep_visible_words_and_exclude_crop_hidden_words_at_every_rotation() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let visible = ["VisibleHeader", "VisibleBody", "VisibleMiddle", "VisibleLower", "VisibleBottom"];
        let hidden = ["HiddenAbove", "HiddenBelow"];
        for original_rotation in [0, 90, 180, 270] {
            let path = folder.path().join(format!("cropped-text-{original_rotation}.pdf"));
            let bytes = crate::text_geometry::tests::fixture(original_rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 372.0], "HiddenAbove\n\nVisibleHeader\nVisibleBody\nVisibleMiddle\nVisibleLower\nVisibleBottom\n\n\n\nHiddenBelow");
            std::fs::write(&path, &bytes).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path.clone(), tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
            for edited_rotation in 0..4 {
                let revision = edited_rotation as u64;
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, revision, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
                assert_eq!(geometry.status, "ok", "{:?}", geometry.reason);
                let geometry_text = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
                for word in visible { assert!(geometry_text.contains(word), "Geometry lost {word}: original {original_rotation}, edited {edited_rotation}"); }
                for word in hidden { assert!(!geometry_text.contains(word), "Geometry copied hidden {word}: original {original_rotation}, edited {edited_rotation}"); }
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, 0, revision, tx)).unwrap(); let text = rx.blocking_recv().unwrap().unwrap();
                for word in visible { assert!(text.contains(word), "Page text lost {word}: original {original_rotation}, edited {edited_rotation}"); }
                for word in hidden { assert!(!text.contains(word), "Page text copied hidden {word}: original {original_rotation}, edited {edited_rotation}"); }
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 260, tx)).unwrap(); let preview = image::load_from_memory(&rx.blocking_recv().unwrap().unwrap()).unwrap();
                let turns = (original_rotation / 90 + edited_rotation) % 4;
                assert_eq!(preview.width() < preview.height(), turns % 2 == 1, "Text inspection changed page rotation");
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
                if edited_rotation < 3 {
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, 0, revision, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
                }
            }
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, 0, 3, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        }
    }
    #[test]
    fn split_validates_every_fixture_page_and_preserves_source_revision_and_undo() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let output = tempfile::tempdir().unwrap();
        for (fixture, count, group) in [("resources/welcome.pdf", 6usize, 2usize), ("../test-corpus/synthetic-scan-98.pdf", 98, 40), ("../test-corpus/synthetic-text-1500.pdf", 1500, 700)] {
            let source_path = root.join(fixture); let source = std::fs::read(&source_path).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(source_path.clone(), tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
            for edit in [PageEdit::Move { from: 0, to: 2 }, PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, edit, tx)).unwrap(); let changed = rx.blocking_recv().unwrap().unwrap(); assert!(changed.dirty);
            }
            let folder = output.path().join(format!("fixture-{count}"));
            let split = |revision, pages_per_file, folder: PathBuf| {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Split(info.id, revision, pages_per_file, folder, tx)).unwrap(); rx.blocking_recv().unwrap()
            };
            assert!(split(0, group, folder.clone()).unwrap_err().contains("changed")); assert!(!folder.exists());
            assert!(split(3, 0, folder.clone()).is_err()); assert!(!folder.exists());
            if count > 64 { assert!(split(3, 1, folder.clone()).unwrap_err().contains("64")); assert!(!folder.exists()); }
            let started = std::time::Instant::now();
            let result = split(3, group, folder.clone()).unwrap();
            println!("split fixture {count}: {} outputs, {} pages, {:?}", result.files.len(), count - 1, started.elapsed());
            assert_eq!(result.files.len(), 3); assert_eq!(result.folder, folder.canonicalize().unwrap());
            let mut emitted = 0;
            for file in result.files {
                assert_eq!(file.first_page, emitted + 1); assert_eq!(file.last_page, (emitted + group).min(count - 1)); assert_eq!(file.page_count, file.last_page - file.first_page + 1);
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(file.path.clone(), tx)).unwrap(); let part = rx.blocking_recv().unwrap().unwrap(); assert_eq!(part.pages.len(), file.page_count);
                for local in 0..part.pages.len() {
                    let position = emitted + local;
                    let source_page = match position { 0 => 1, 1 => 0, other => other + 1 };
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(part.id, local as u16, 0, tx)).unwrap(); let text = rx.blocking_recv().unwrap().unwrap();
                    assert!(text.contains(&format!("Page {} of {count}", source_page + 1)), "Incorrect split order: {fixture}, position {position}");
                }
                if emitted == 0 {
                    assert_eq!(part.pages[0].width, info.pages[1].height); assert_eq!(part.pages[0].height, info.pages[1].width);
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 260, tx)).unwrap(); let current = rx.blocking_recv().unwrap().unwrap();
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(part.id, 0, 260, tx)).unwrap(); let copied = rx.blocking_recv().unwrap().unwrap();
                    assert_eq!(image::load_from_memory(&current).unwrap().into_rgba8(), image::load_from_memory(&copied).unwrap().into_rgba8(), "Split rotation/render differs from current edited page: {fixture}");
                }
                emitted += part.pages.len();
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(part.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            }
            assert_eq!(emitted, count - 1); assert_eq!(std::fs::read(&source_path).unwrap(), source);
            assert!(split(3, group, folder.clone()).unwrap_err().contains("already exists"));
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, 0, 3, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().unwrap().contains(&format!("Page 2 of {count}")));
            for undo in 0..3 {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Undo, tx)).unwrap(); let changed = rx.blocking_recv().unwrap().unwrap();
                assert_eq!(changed.revision, 4 + undo); assert_eq!(changed.pages.len(), count); assert_eq!(changed.dirty, undo != 2); assert!(changed.can_redo); assert_eq!(changed.can_undo, undo != 2);
            }
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            let closed = output.path().join(format!("closed-{count}")); assert!(split(6, group, closed.clone()).unwrap_err().contains("closed")); assert!(!closed.exists());
        }
    }
    #[test]
    fn text_geometry_preserves_spaces_generated_newlines_and_unicode() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("whitespace-unicode.pdf");
        std::fs::write(&path, crate::text_geometry::tests::fixture(0, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "First café\nSecond line")).unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, 0, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(geometry.status, "ok", "{:?}", geometry.reason);
        let copied = geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>();
        assert!(copied.contains("First café")); assert!(copied.contains("Second line")); assert!(copied.contains('\n'));
        assert!(geometry.characters.iter().any(|character| character.text == "é" && character.bounds.is_some()));
        assert!(geometry.characters.iter().filter(|character| character.text == "\r" || character.text == "\n").all(|character| character.bounds.is_none()));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, 0, 0, tx)).unwrap(); let source = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(copied, source);
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
    }
    #[test]
    fn text_geometry_cropped_rotated_glyphs_match_actual_rendered_ink() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        for rotation in [0, 90, 180, 270] {
            let path = folder.path().join(format!("ink-{rotation}.pdf"));
            std::fs::write(&path, crate::text_geometry::tests::fixture(rotation, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "MMMM")).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
            for edited in 0..4 {
                let total = (rotation / 90 + edited) % 4;
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 900, tx)).unwrap(); let before = rx.blocking_recv().unwrap().unwrap();
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, edited as u64, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
                assert_eq!(geometry.status, "ok", "{:?}", geometry.reason);
                assert_eq!(geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>(), "MMMM");
                assert!(geometry.characters.iter().all(|character| character.angle as i64 == total * 90));
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 901, tx)).unwrap(); let after = rx.blocking_recv().unwrap().unwrap();
                let image = image::load_from_memory(&before).unwrap().into_rgb8();
                let after = image::load_from_memory(&after).unwrap().into_rgb8();
                assert_eq!(image.width() < image.height(), after.width() < after.height());
                let ink: Vec<_> = image.enumerate_pixels().filter(|(_, _, pixel)| pixel.0.iter().any(|value| *value < 100)).map(|(x, y, _)| (x as f32, y as f32)).collect();
                assert!(!ink.is_empty());
                let boxes: Vec<_> = geometry.characters.iter().map(|character| {
                    let bounds = character.bounds.as_ref().unwrap();
                    (bounds.x * image.width() as f32, bounds.y * image.height() as f32, (bounds.x + bounds.width) * image.width() as f32, (bounds.y + bounds.height) * image.height() as f32)
                }).collect();
                let inside = |(x, y): (f32, f32), (left, top, right, bottom): (f32, f32, f32, f32)| x >= left - 2.0 && x <= right + 2.0 && y >= top - 2.0 && y <= bottom + 2.0;
                for bounds in &boxes { assert!(ink.iter().any(|point| inside(*point, *bounds)), "No rendered ink in mapped character: rotation {rotation}, edit {edited}"); }
                assert!(ink.iter().all(|point| boxes.iter().any(|bounds| inside(*point, *bounds))), "Rendered ink falls outside mapped characters: rotation {rotation}, edit {edited}");
                let after_boxes: Vec<_> = geometry.characters.iter().map(|character| {
                    let bounds = character.bounds.as_ref().unwrap();
                    (bounds.x * after.width() as f32, bounds.y * after.height() as f32, (bounds.x + bounds.width) * after.width() as f32, (bounds.y + bounds.height) * after.height() as f32)
                }).collect();
                assert!(after.enumerate_pixels().filter(|(_, _, pixel)| pixel.0.iter().any(|value| *value < 100)).all(|(x, y, _)| after_boxes.iter().any(|bounds| inside((x as f32, y as f32), *bounds))), "Geometry changed source rotation before a fresh render: rotation {rotation}, edit {edited}");
                if edited < 3 { let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap(); }
            }
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
    }
    #[test]
    fn text_geometry_unsupported_is_all_or_nothing_and_cap_is_explicit() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        for (index, matrix) in [[1.0, 0.0, 0.2, 1.0, 100.0, 200.0], [0.707, 0.707, -0.707, 0.707, 100.0, 200.0], [-1.0, 0.0, 0.0, 1.0, 200.0, 200.0]].into_iter().enumerate() {
            let path = folder.path().join(format!("unsupported-{index}.pdf"));
            std::fs::write(&path, crate::text_geometry::tests::fixture(90, matrix, "MMMM")).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, 1, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
            assert_eq!(geometry.status, "unsupported"); assert!(geometry.characters.is_empty()); assert!(geometry.reason.is_some());
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 900, tx)).unwrap(); let image = image::load_from_memory(&rx.blocking_recv().unwrap().unwrap()).unwrap();
            assert!(image.width() > image.height(), "Unsupported geometry must preserve original /Rotate90 plus editRotate90");
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
        for count in [20_000, 20_001] {
            let path = folder.path().join(format!("cap-{count}.pdf"));
            std::fs::write(&path, crate::text_geometry::tests::fixture(0, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], &"M".repeat(count))).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, 0, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
            assert_eq!(geometry.status, "ok", "{:?}", geometry.reason); assert_eq!(geometry.truncated, count > 20_000); assert!(geometry.characters.len() <= 20_000); assert!(serde_json::to_vec(&geometry).unwrap().len() < 4_000_000);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
    }
    #[test]
    fn geometry_follows_edits_and_rejects_stale_invalid_and_closed_requests() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (fixture, count) in [("resources/welcome.pdf", 6), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(root.join(fixture), tx)).unwrap();
            let info = rx.blocking_recv().unwrap().unwrap();
            let read = |page, revision| {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, page, revision, tx)).unwrap(); rx.blocking_recv().unwrap()
            };
            for page in [0, count / 2, count - 1] {
                let geometry = read(page, 0).unwrap();
                assert_eq!(geometry.status, "ok", "{fixture}: {:?}", geometry.reason);
                assert!(geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>().contains(&format!("Page {} of {count}", page + 1)));
            }
            assert!(read(count, 0).is_err());
            for edit in [PageEdit::Move { from: 0, to: 1 }, PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, edit, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            }
            assert!(read(0, 0).is_err());
            let geometry = read(0, 3).unwrap();
            assert!(geometry.characters.iter().map(|character| character.text.as_str()).collect::<String>().contains(&format!("Page 2 of {count}")));
            assert!(geometry.characters.iter().filter(|character| character.bounds.is_some()).all(|character| character.angle == 90));
            assert!(read(count - 1, 3).is_err());
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            assert!(read(0, 3).is_err());
        }
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("rotated.pdf");
        std::fs::write(&path, crate::text_geometry::tests::fixture(90, [1.0, 0.0, 0.0, 1.0, 100.0, 200.0], "MMMM")).unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::TextGeometry(info.id, 0, 1, tx)).unwrap(); let geometry = rx.blocking_recv().unwrap().unwrap();
        assert!(geometry.characters.iter().all(|character| character.angle == 180));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
    }
    #[test]
    fn properties_follow_current_pages_without_modifying_source_metadata() {
        use lopdf::{dictionary, Object};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        let metadata = pdf.add_object(dictionary! { "Title" => Object::string_literal("<script>plain PDF title</script>"), "Author" => Object::string_literal("Fixture author") });
        pdf.trailer.set("Info", metadata);
        let path = folder.path().join("properties.pdf"); pdf.save(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path.clone(), tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Properties(info.id, 0, tx)).unwrap(); let original = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(original.page_count, 6); assert_eq!(original.source_size_bytes, bytes.len());
        assert_eq!(original.security.encrypted, Some(false)); assert_eq!(original.signature_validation, "not_performed");
        assert_eq!(original.metadata.iter().find(|entry| entry.name == "title").unwrap().value, "<script>plain PDF title</script>");
        for edit in [PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Delete { pages: vec![1] }] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, edit, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Properties(info.id, 0, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Properties(info.id, 2, tx)).unwrap(); let edited = rx.blocking_recv().unwrap().unwrap();
        assert_eq!(edited.page_count, 5); assert_eq!(edited.source_size_bytes, bytes.len());
        assert_eq!(edited.page_dimensions.iter().map(|size| size.count).sum::<usize>(), 5);
        assert_eq!(edited.page_dimensions[0].width_points, info.pages[0].height);
        assert_eq!(edited.page_dimensions[0].height_points, info.pages[0].width);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Properties(info.id, 2, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
    }
    #[test]
    fn print_dimensions_bound_pixels_and_preserve_aspect() {
        for (width, height) in [(612.0, 792.0), (792.0, 612.0), (1.0, 1.0), (1.0, 1_000_000.0), (1_000_000.0, 1.0)] {
            for (max_width, max_height) in [(300, 400), (1, 1), (u32::MAX, u32::MAX)] {
                let (output_width, output_height) = print_dimensions(width, height, max_width, max_height).unwrap();
                assert!(output_width > 0 && output_height > 0);
                assert!(output_width <= max_width.min(4096) && output_height <= max_height.min(4096));
                assert!(output_width as u64 * output_height as u64 <= 16_000_000);
            }
        }
        assert_eq!(print_dimensions(600.0, 800.0, 300, 300).unwrap(), (225, 300));
        assert_eq!(print_dimensions(800.0, 600.0, 300, 300).unwrap(), (300, 225));
        for (width, height, max_width, max_height) in [(0.0, 1.0, 10, 10), (f32::NAN, 1.0, 10, 10), (1.0, f32::INFINITY, 10, 10), (1.0, 1.0, 0, 10)] { assert!(print_dimensions(width, height, max_width, max_height).is_err()); }
    }
    #[test]
    fn print_bitmap_is_bgra_with_opaque_white_background() {
        let _print_lock = print_test_lock();
        use lopdf::{dictionary, Object, Stream};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let mut pdf = lopdf::Document::with_version("1.7");
        let pages = pdf.new_object_id();
        let content = pdf.add_object(Stream::new(dictionary! {}, b"1 0 0 rg 50 50 100 100 re f".to_vec()));
        let page = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()], "Contents" => content });
        pdf.objects.insert(pages, dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1 }.into());
        let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages }); pdf.trailer.set("Root", catalog);
        let path = folder.path().join("print-colors.pdf"); pdf.save(&path).unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap(); let info = rx.blocking_recv().unwrap().unwrap();
        let snapshot = print_snapshot(&service, info.id, 0);
        let bitmap = service.print_render_blocking(snapshot.token, 0, 100, 100).unwrap();
        assert_eq!(&bitmap.bgra[..4], &[255, 255, 255, 255]);
        let center = (50 * bitmap.width as usize + 50) * 4; assert_eq!(&bitmap.bgra[center..center + 4], &[0, 0, 255, 255]);
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::EndPrint(snapshot.token, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
    }
    #[test]
    fn print_snapshot_survives_edits_and_close_but_not_release() {
        let _print_lock = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(root.join("resources/welcome.pdf"), tx)).unwrap();
        let info = rx.blocking_recv().unwrap().unwrap();
        let before = print_snapshot(&service, info.id, 0); assert_eq!(before.pages, 6);
        let original_first = service.print_render_blocking(before.token, 0, 300, 400).unwrap();
        let original_second = service.print_render_blocking(before.token, 1, 300, 400).unwrap();
        assert_eq!(original_first.bgra.len(), original_first.width as usize * original_first.height as usize * 4);
        assert!(original_first.bgra.chunks_exact(4).all(|pixel| pixel[3] == 255));
        assert!(original_first.bgra.chunks_exact(4).any(|pixel| pixel == [255, 255, 255, 255]));
        assert!(service.print_render_blocking(before.token, 6, 300, 400).is_err());
        assert!(service.print_render_blocking(before.token, 0, 0, 400).is_err());
        for edit in [PageEdit::Rotate { pages: vec![0], clockwise: true }, PageEdit::Move { from: 0, to: 2 }] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, edit, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginPrint(info.id, 0, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        let after = print_snapshot(&service, info.id, 2);
        let reordered = service.print_render_blocking(after.token, 0, 300, 400).unwrap(); assert_eq!(reordered.bgra, original_second.bgra);
        let rotated = service.print_render_blocking(after.token, 2, 300, 400).unwrap(); assert!(rotated.width > rotated.height);
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        let pinned = service.print_render_blocking(before.token, 0, 300, 400).unwrap(); assert_eq!(pinned.bgra, original_first.bgra);
        let pinned_rotated = service.print_render_blocking(after.token, 2, 300, 400).unwrap(); assert_eq!(pinned_rotated.bgra, rotated.bgra);
        for token in [before.token, after.token] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::EndPrint(token, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            assert!(service.print_render_blocking(token, 0, 300, 400).is_err());
        }
    }
    #[test]
    fn encrypted_open_retries_cancels_and_keeps_edits_blocked() {
        let _print_lock = print_test_lock();
        let _password_lock = password_test_lock();
        use lopdf::encryption::crypt_filters::{Aes128CryptFilter, Aes256CryptFilter, CryptFilter};
        use std::{collections::BTreeMap, sync::Arc};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let ordinary = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        for (version, user_password) in [2, 4, 5].into_iter().flat_map(|version| ["test password", ""].map(|password| (version, password))) {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            pdf.trailer.set("ID", vec![lopdf::Object::string_literal("password-test-id"), lopdf::Object::string_literal("password-test-id")]);
            let permissions = lopdf::Permissions::all();
            let fixture_key = [0x42u8; 32];
            let encryption = match version {
                2 => lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner password", user_password, key_length: 128, permissions },
                4 => {
                    let filter: Arc<dyn CryptFilter> = Arc::new(Aes128CryptFilter);
                    lopdf::EncryptionVersion::V4 { document: &pdf, encrypt_metadata: true, crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]), stream_filter: b"StdCF".to_vec(), string_filter: b"StdCF".to_vec(), owner_password: "owner password", user_password, permissions }
                }
                5 => {
                    let filter: Arc<dyn CryptFilter> = Arc::new(Aes256CryptFilter);
                    lopdf::EncryptionVersion::V5 { encrypt_metadata: true, crypt_filters: BTreeMap::from([(b"StdCF".to_vec(), filter)]), file_encryption_key: &fixture_key, stream_filter: b"StdCF".to_vec(), string_filter: b"StdCF".to_vec(), owner_password: "owner password", user_password, permissions }
                }
                _ => unreachable!(),
            };
            let state = lopdf::EncryptionState::try_from(encryption).unwrap();
            pdf.encrypt(&state).unwrap();
            let kind = if user_password.is_empty() { "empty" } else { "locked" };
            let path = folder.path().join(format!("v{version}-{kind}.pdf"));
            pdf.save(&path).unwrap();
            let source = std::fs::read(&path).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginOpen(path.clone(), tx)).unwrap();
            let result = rx.blocking_recv().unwrap().unwrap().accept();
            let info = if user_password.is_empty() {
                match result { OpenResult::Opened { document, .. } => document, _ => panic!("Empty password should open") }
            } else {
                let request_id = match result { OpenResult::PasswordRequired { request_id, incorrect, .. } => { assert!(!incorrect); request_id }, _ => panic!("Expected password challenge") };
                for wrong in ["wrong", ""] {
                    let (tx, rx) = oneshot::channel(); service.sender.send(Request::Unlock(request_id, wrong.into(), tx)).unwrap();
                    assert!(matches!(rx.blocking_recv().unwrap().unwrap().accept(), OpenResult::PasswordRequired { .. }));
                }
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Unlock(request_id, user_password.into(), tx)).unwrap();
                let opened = match rx.blocking_recv().unwrap().unwrap().accept() { OpenResult::Opened { document, .. } => document, _ => panic!("Correct password failed") };
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Unlock(request_id, user_password.into(), tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginOpen(path.clone(), tx)).unwrap();
                let cancelled = match rx.blocking_recv().unwrap().unwrap().accept() { OpenResult::PasswordRequired { request_id, .. } => request_id, _ => panic!("Expected challenge") };
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::CancelPassword(cancelled, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Unlock(cancelled, user_password.into(), tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
                opened
            };
            assert_eq!(info.pages.len(), 6);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Properties(info.id, 0, tx)).unwrap(); let properties = rx.blocking_recv().unwrap().unwrap();
            assert_ne!(properties.security.encrypted, Some(false), "Encrypted v{version}-{kind} must never be reported unencrypted");
            assert_eq!(properties.page_count, 6); assert_eq!(properties.source_size_bytes, source.len());
            assert!(call(&service, |reply| Request::Text(info.id, 0, 0, reply)).is_ok(), "v{version}-{kind} copy permission must allow Page text");
            assert_eq!(call(&service, |reply| Request::TextGeometry(info.id, 0, 0, reply)).unwrap().status, "ok", "v{version}-{kind} copy permission must allow text geometry");
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginPrint(info.id, 0, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err(), "Encrypted v{version}-{kind} must not print");
            let comments = call(&service, |reply| Request::Comments(info.id, 0, reply)).unwrap(); assert_eq!(comments.status, "unsupported"); assert!(comments.notes.is_empty());
            assert_eq!(call(&service, |reply| Request::FormFields(info.id, 0, reply)).unwrap().status, "unsupported", "Encrypted v{version}-{kind} must not expose fillable fields");
            let form_output = folder.path().join("must-not-fill.pdf"); assert!(call(&service, |reply| Request::FillFormCopy(info.id, 0, vec![crate::forms::FieldValue::Text {field_id:"unknown".into(),value:"blocked".into()}], form_output.clone(), reply)).is_err()); assert!(!form_output.exists());
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::Create(0, CropRect { x: 0.1, y: 0.1, width: 0.1, height: 0.1 }, "Blocked encrypted note".into()), reply)).is_err(), "Encrypted v{version}-{kind} must not create notes");
            assert_eq!(call(&service, |reply| Request::Annotations(info.id, 0, reply)).unwrap().status, "unsupported"); assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateHighlight(0, CropRect { x: 0.1, y: 0.1, width: 0.1, height: 0.1 }, None), reply)).is_err(), "Encrypted v{version}-{kind} must not create highlights");
            assert!(call(&service, |reply| Request::Comment(info.id, 0, CommentMutation::CreateTextHighlight(0, 0, 1, None), reply)).is_err(), "Encrypted v{version}-{kind} must not create text highlights");
            for page in 0..info.pages.len() {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, page as u16, 100, tx)).unwrap(); assert!(!rx.blocking_recv().unwrap().unwrap().is_empty(), "v{version}-{kind} page {page}");
            }
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Crop(info.id, 0, 0, CropRect { x: 0.1, y: 0.1, width: 0.8, height: 0.8 }, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err(), "Encrypted v{version}-{kind} must not crop");
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 101, tx)).unwrap(); assert!(!rx.blocking_recv().unwrap().unwrap().is_empty(), "Rejected crop must preserve a fresh encrypted preview");
            let output = folder.path().join("must-not-export.pdf");
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Save(info.id, None, output.clone(), tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err()); assert!(!output.exists());
            assert!(call(&service, |reply| Request::CheckCombine(combine_source(&ordinary), combine_source(&info), reply)).is_err(), "Encrypted v{version}-{kind} must not combine");
            assert!(call(&service, |reply| Request::Combine(combine_source(&info), combine_source(&ordinary), output.clone(), reply)).is_err()); assert!(!output.exists());
            for (target, donor) in [(&ordinary, &info), (&info, &ordinary)] {
                assert!(call(&service, |reply| Request::CheckInsertion(combine_source(target), combine_source(donor), 0, reply)).is_err(), "Encrypted v{version}-{kind} must not insert");
                assert!(call(&service, |reply| Request::InsertPages(combine_source(target), combine_source(donor), 0, output.clone(), reply)).is_err()); assert!(!output.exists());
                assert!(call(&service, |reply| Request::CheckReplacement(combine_source(target), combine_source(donor), 0, target.pages.len(), reply)).is_err(), "Encrypted v{version}-{kind} must not replace even the entire target");
                assert!(call(&service, |reply| Request::ReplacePages(combine_source(target), combine_source(donor), 0, target.pages.len(), output.clone(), reply)).is_err()); assert!(!output.exists());
            }
            assert_eq!(std::fs::read(&path).unwrap(), source);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        }
        call(&service, |reply| Request::Close(ordinary.id, reply)).unwrap();
    }
    #[test]
    fn malformed_middle_page_is_rejected_instead_of_silently_truncated() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        let middle = pdf.get_pages()[&2];
        pdf.objects.insert(middle, lopdf::Object::Null);
        let path = folder.path().join("broken-page.pdf"); pdf.save(&path).unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::BeginOpen(path.clone(), tx)).unwrap();
        assert!(rx.blocking_recv().unwrap().err().unwrap().contains("page 2"));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap();
        assert!(rx.blocking_recv().unwrap().err().unwrap().contains("page 2"));
    }
    #[test]
    fn bookmarks_include_nested_destinations_and_disable_external_actions() {
        use lopdf::{dictionary, Object};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        let pages = pdf.get_pages();
        let outlines = pdf.new_object_id();
        let first = pdf.new_object_id();
        let child = pdf.new_object_id();
        let external = pdf.new_object_id();
        pdf.objects.insert(outlines, dictionary! { "Type" => "Outlines", "First" => first, "Last" => external, "Count" => 3 }.into());
        pdf.objects.insert(first, dictionary! { "Title" => Object::string_literal("Start"), "Parent" => outlines, "Next" => external, "First" => child, "Last" => child, "Count" => 1, "Dest" => vec![Object::Reference(pages[&1]), Object::Name(b"Fit".to_vec())] }.into());
        pdf.objects.insert(child, dictionary! { "Title" => Object::string_literal("Second page"), "Parent" => first, "A" => dictionary! { "S" => "GoTo", "D" => vec![Object::Reference(pages[&2]), Object::Name(b"Fit".to_vec())] } }.into());
        pdf.objects.insert(external, dictionary! { "Title" => Object::string_literal("External"), "Parent" => outlines, "Prev" => first, "A" => dictionary! { "S" => "URI", "URI" => Object::string_literal("https://example.com") } }.into());
        pdf.catalog_mut().unwrap().set("Outlines", outlines);
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("bookmarks.pdf"); pdf.save(&path).unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap();
        let info = rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Bookmarks(info.id, 0, tx)).unwrap();
        let result = rx.blocking_recv().unwrap().unwrap();
        assert!(!result.truncated); assert_eq!(result.items.len(), 3);
        assert_eq!(result.items[0].title, "Start"); assert_eq!(result.items[0].page, Some(0));
        assert_eq!(result.items[1].depth, 1); assert_eq!(result.items[1].page, Some(1));
        assert_eq!(result.items[2].page, None);
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Bookmarks(info.id, 99, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
    }
    #[test]
    fn bookmark_cycles_and_large_outlines_are_bounded() {
        use lopdf::{dictionary, Object};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (count, cycle) in [(0, false), (3, true), (1005, false)] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            if count > 0 {
                let page = pdf.get_pages()[&1];
                let outlines = pdf.new_object_id();
                let ids: Vec<_> = (0..count).map(|_| pdf.new_object_id()).collect();
                pdf.objects.insert(outlines, dictionary! { "Type" => "Outlines", "First" => ids[0], "Last" => ids[count - 1], "Count" => count as i64 }.into());
                for (index, id) in ids.iter().enumerate() {
                    let mut entry = dictionary! { "Title" => Object::string_literal(format!("Bookmark {index}")), "Parent" => outlines, "Dest" => vec![Object::Reference(page), Object::Name(b"Fit".to_vec())] };
                    if index > 0 { entry.set("Prev", ids[index - 1]); }
                    if index + 1 < count { entry.set("Next", ids[index + 1]); }
                    else if cycle { entry.set("Next", ids[0]); }
                    pdf.objects.insert(*id, entry.into());
                }
                pdf.catalog_mut().unwrap().set("Outlines", outlines);
            }
            let path = folder.path().join(format!("outline-{count}.pdf")); pdf.save(&path).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap();
            let info = rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Bookmarks(info.id, 0, tx)).unwrap();
            let result = rx.blocking_recv().unwrap().unwrap();
            assert_eq!(result.items.len(), count.min(1000));
            assert_eq!(result.truncated, count > 1000);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 64, tx)).unwrap();
            assert!(rx.blocking_recv().unwrap().is_ok());
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Bookmarks(info.id, 0, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        }
    }
    #[test]
    fn bookmarks_reject_unbounded_title_text() {
        use lopdf::{dictionary, Object};
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        let page = pdf.get_pages()[&1];
        let outlines = pdf.new_object_id();
        let bookmark = pdf.new_object_id();
        pdf.objects.insert(outlines, dictionary! { "Type" => "Outlines", "First" => bookmark, "Last" => bookmark, "Count" => 1 }.into());
        pdf.objects.insert(bookmark, dictionary! { "Title" => Object::string_literal("B".repeat(1_048_577)), "Parent" => outlines, "Dest" => vec![Object::Reference(page), Object::Name(b"Fit".to_vec())] }.into());
        pdf.catalog_mut().unwrap().set("Outlines", outlines);
        let path = folder.path().join("oversized-bookmark-title.pdf");
        pdf.save(&path).unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(path, reply)).unwrap();
        let (reply, receiver) = oneshot::channel();
        drop(receiver);
        service.sender.send(Request::Bookmarks(info.id, info.revision, reply)).unwrap();
        let skipped = call(&service, |reply| Request::BookmarkWorkForDocument(info.id, reply)).unwrap();
        assert_eq!((skipped.dequeued, skipped.visited), (1, 0));
        let error = match call(&service, |reply| Request::Bookmarks(info.id, info.revision, reply)) { Ok(_) => panic!("oversized bookmark text was returned"), Err(error) => error };
        assert_eq!(error, "Bookmarks contain too much text.");
        let bounded = call(&service, |reply| Request::BookmarkWorkForDocument(info.id, reply)).unwrap();
        assert_eq!((bounded.dequeued, bounded.visited), (1, 1));
    }
    #[test]
    fn page_text_rejects_unbounded_visible_text() {
        use lopdf::Object;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let line = "M".repeat(32_000);
        let text = vec![line.as_str(); 40].join("\n");
        let mut pdf = lopdf::Document::load_mem(&crate::text_geometry::tests::fixture(0, [1.0, 0.0, 0.0, 1.0, 100.0, 2_000_000.0], &text)).unwrap();
        let page = pdf.get_pages()[&1];
        let bounds = vec![0.into(), 0.into(), Object::Real(100_000_000.0), Object::Real(4_000_000.0)];
        pdf.get_dictionary_mut(page).unwrap().set("MediaBox", bounds.clone());
        pdf.get_dictionary_mut(page).unwrap().set("CropBox", bounds);
        let path = folder.path().join("oversized-visible-page-text.pdf");
        pdf.save(&path).unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(path, reply)).unwrap();
        let error = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap_err();
        assert_eq!(error, "This page has too much embedded text to read or search.");
        let cropped_path = folder.path().join("oversized-hidden-page-text.pdf");
        pdf.get_dictionary_mut(page).unwrap().set("CropBox", vec![0.into(), Object::Real(1_999_990.0), Object::Real(1_000_000.0), Object::Real(2_000_030.0)]);
        pdf.save(&cropped_path).unwrap();
        let cropped = call(&service, |reply| Request::Open(cropped_path, reply)).unwrap();
        let visible = call(&service, |reply| Request::Text(cropped.id, 0, cropped.revision, reply)).unwrap();
        assert!(!visible.is_empty() && visible.len() < MAX_READ_TEXT_BYTES, "visible cropped bytes: {}", visible.len());
        for id in [info.id, cropped.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
    }
    #[test]
    fn text_reads_enforce_extraction_permission_without_rejecting_allowed_encryption() {
        let _password_guard = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (kind, permissions, user_password, allowed) in [
            ("denied", lopdf::Permissions::empty(), "", false),
            ("accessibility-only", lopdf::Permissions::COPYABLE_FOR_ACCESSIBILITY, "restricted", false),
            ("copy-only", lopdf::Permissions::COPYABLE, "copy", true),
            ("allowed", lopdf::Permissions::all(), "", true),
        ] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            pdf.trailer.set("ID", vec![lopdf::Object::string_literal(format!("text-{kind}")), lopdf::Object::string_literal(format!("text-{kind}"))]);
            let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner", user_password, key_length: 128, permissions };
            pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
            let path = folder.path().join(format!("text-{kind}.pdf"));
            pdf.save(&path).unwrap();
            let source = std::fs::read(&path).unwrap();
            let opened = call(&service, |reply| Request::BeginOpen(path.clone(), reply)).unwrap();
            let info = match opened {
                OpenResult::Opened { document, .. } => document,
                OpenResult::PasswordRequired { request_id, .. } => match call(&service, |reply| Request::Unlock(request_id, user_password.into(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{kind} did not unlock") },
            };
            if !allowed {
                for error in [
                    call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap_err(),
                    call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap_err(),
                ] { assert_eq!(error, "This PDF does not allow text and graphics extraction."); }
            } else {
                call(&service, |reply| Request::Text(info.id, 0, info.revision, reply)).unwrap();
                assert_eq!(call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply)).unwrap().status, "ok");
            }
            assert_eq!(std::fs::read(&path).unwrap(), source);
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
        let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
        pdf.trailer.set("ID", vec![lopdf::Object::string_literal("text-owner"), lopdf::Object::string_literal("text-owner")]);
        let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner", user_password: "reader", key_length: 128, permissions: lopdf::Permissions::empty() };
        pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
        let path = folder.path().join("text-owner.pdf");
        pdf.save(&path).unwrap();
        let source = std::fs::read(&path).unwrap();
        for (password, owner_access) in [("reader", false), ("owner", true)] {
            let opened = call(&service, |reply| Request::BeginOpen(path.clone(), reply)).unwrap();
            let request_id = match opened { OpenResult::PasswordRequired { request_id, .. } => request_id, _ => panic!("protected fixture opened without a password") };
            let info = match call(&service, |reply| Request::Unlock(request_id, password.into(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{password} did not unlock") };
            let text = call(&service, |reply| Request::Text(info.id, 0, info.revision, reply));
            let geometry = call(&service, |reply| Request::TextGeometry(info.id, 0, info.revision, reply));
            if owner_access { assert!(text.is_ok() && geometry.is_ok()); } else { assert_eq!(text.unwrap_err(), "This PDF does not allow text and graphics extraction."); assert_eq!(geometry.unwrap_err(), "This PDF does not allow text and graphics extraction."); }
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
        assert_eq!(std::fs::read(&path).unwrap(), source);
    }
    #[test]
    fn read_text_byte_limit_is_checked_without_overflow() {
        assert_eq!(checked_read_text_bytes(MAX_READ_TEXT_BYTES - 1, 1, "limit").unwrap(), MAX_READ_TEXT_BYTES);
        for (current, additional) in [(MAX_READ_TEXT_BYTES, 1), (usize::MAX, 1)] {
            assert_eq!(checked_read_text_bytes(current, additional, "limit").unwrap_err(), "limit");
        }
    }
    #[test]
    fn text_follows_page_edits_and_rejects_stale_or_closed_requests() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (fixture, count) in [("resources/welcome.pdf", 6), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(root.join(fixture), tx)).unwrap();
            let info = rx.blocking_recv().unwrap().unwrap();
            let read = |page, revision| {
                let (tx, rx) = oneshot::channel(); service.sender.send(Request::Text(info.id, page, revision, tx)).unwrap();
                rx.blocking_recv().unwrap()
            };
            for page in 0..count {
                assert!(read(page, 0).unwrap().contains(&format!("Page {} of {count}", page + 1)));
            }
            assert!(read(count, 0).is_err());
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Move { from: 0, to: 1 }, tx)).unwrap();
            let changed = rx.blocking_recv().unwrap().unwrap();
            assert!(read(0, 0).is_err());
            assert!(read(0, changed.revision).unwrap().contains(&format!("Page 2 of {count}")));
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
            assert!(read(0, changed.revision).is_err());
        }
    }
    #[test]
    fn ocr_raster_matches_current_print_pixels_and_preserves_worker_state() {
        let _ocr_guard = crate::ocr_process::test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source_path = root.join("resources/welcome.pdf");
        let source_bytes = std::fs::read(&source_path).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let mut info = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.05, y: 0.05, width: 0.9, height: 0.8 }, reply)).unwrap();
        info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, CropRect { x: 0.68, y: 0.08, width: 0.18, height: 0.12 }, Some("OCR raster note".into())), reply)).unwrap();
        let annotations = call(&service, |reply| Request::Annotations(info.id, info.revision, reply)).unwrap();
        assert!(annotations.annotations.iter().any(|annotation| annotation.contents.as_deref() == Some("OCR raster note")));
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Undo, reply)).unwrap();
        assert!(info.can_redo);
        let preview = call(&service, |reply| Request::Render(info.id, 0, 211, reply)).unwrap();
        let before = call(&service, |reply| Request::OcrWorkerState(info.id, reply)).unwrap();
        let request = crate::ocr::OcrPageRequest { document_id: info.id, revision: info.revision, page: 0 };
        let raster = ocr_raster(&service, request, crate::ocr_process::OcrCancellation::default()).unwrap();
        assert_eq!(raster.request, request);
        assert!(raster.p6.len() <= crate::ocr_process::OCR_INPUT_LIMIT);
        let (width, height, pixels) = p6_pixels(&raster.p6);
        assert_eq!((width, height), (raster.width, raster.height));
        let output = folder.path().join("current-ocr-reference.png");
        let png = call(&service, |reply| Request::ExportPageImage(info.id, info.revision, 0, 150, PageImageFormat::Png, output.clone(), reply)).unwrap();
        assert_eq!((png.width, png.height), (width, height));
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(output).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut decoded = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut decoded).unwrap();
        assert_eq!(frame.buffer_size(), pixels.len());
        assert_eq!(&decoded[..frame.buffer_size()], pixels, "OCR P6 must match the independently encoded current-page PNG pixels");
        assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 211, reply)).unwrap(), preview);
        let after = call(&service, |reply| Request::OcrWorkerState(info.id, reply)).unwrap();
        assert_eq!(after, before, "OCR must not change source, plan/history flags, viewer cache, or persistent note cache");
        let work = call(&service, |reply| Request::OcrWorkForDocument(info.id, reply)).unwrap();
        assert_eq!((work.dequeued, work.preflighted, work.rendered, work.converted), (1, 1, 1, 1));
        assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        call(&service, |reply| Request::Close(info.id, reply)).unwrap();

        let form = call(&service, |reply| Request::Open(root.join("tests/fixtures/reportlab-mixed-fields.pdf"), reply)).unwrap();
        assert!(!call(&service, |reply| Request::FormFields(form.id, 0, reply)).unwrap().fields.is_empty());
        let form_request = crate::ocr::OcrPageRequest { document_id: form.id, revision: 0, page: 0 };
        let form_raster = ocr_raster(&service, form_request, crate::ocr_process::OcrCancellation::default()).unwrap();
        let (form_width, form_height, form_pixels) = p6_pixels(&form_raster.p6);
        let snapshot = call(&service, |reply| Request::BeginPrint(form.id, 0, reply)).unwrap();
        let print = call(&service, |reply| Request::PrintRender(snapshot.token, 0, form_width, form_height, reply)).unwrap();
        let form_expected = print.bgra.chunks_exact(4).flat_map(|pixel| {
            let alpha = u16::from(pixel[3]);
            [pixel[2], pixel[1], pixel[0]].map(|channel| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8)
        }).collect::<Vec<_>>();
        assert_eq!(form_pixels, form_expected, "OCR form pixels must match an independent print-quality form render");
        call(&service, |reply| Request::Close(form.id, reply)).unwrap();
    }

    #[test]
    fn ocr_raster_rejects_stale_closed_protected_and_preclosed_requests_atomically() {
        let _ocr_guard = crate::ocr_process::test_lock();
        use lopdf::dictionary;
        let _password_guard = password_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let ordinary = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        for request in [
            crate::ocr::OcrPageRequest { document_id: ordinary.id, revision: 1, page: 0 },
            crate::ocr::OcrPageRequest { document_id: ordinary.id, revision: 0, page: 6 },
        ] {
            assert!(ocr_raster(&service, request, crate::ocr_process::OcrCancellation::default()).is_err());
        }
        let invalid = call(&service, |reply| Request::OcrWorkForDocument(ordinary.id, reply)).unwrap();
        assert_eq!((invalid.dequeued, invalid.preflighted, invalid.rendered, invalid.converted), (2, 0, 0, 0));
        let cancellation = crate::ocr_process::OcrCancellation::default();
        cancellation.cancel();
        assert!(ocr_raster(&service, crate::ocr::OcrPageRequest { document_id: ordinary.id, revision: 0, page: 0 }, cancellation).is_err());

        let cancellation = crate::ocr_process::OcrCancellation::default();
        let operation = crate::ocr_process::OcrOperation::try_begin(&cancellation).unwrap();
        let (reply, receiver) = oneshot::channel(); drop(receiver);
        service.sender.send(Request::OcrRaster(crate::ocr::OcrPageRequest { document_id: ordinary.id, revision: 0, page: 0 }, cancellation, operation.worker_hold(), reply)).unwrap();
        let skipped = call(&service, |reply| Request::OcrWorkForDocument(ordinary.id, reply)).unwrap();
        operation.finish(Ok(())).unwrap();
        assert_eq!((skipped.dequeued, skipped.preflighted, skipped.rendered, skipped.converted), (1, 0, 0, 0));

        for kind in ["signed", "extraction-denied", "encrypted-allowed"] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            pdf.trailer.set("ID", vec![lopdf::Object::string_literal(format!("ocr-{kind}")), lopdf::Object::string_literal(format!("ocr-{kind}"))]);
            match kind {
                "signed" => { pdf.catalog_mut().unwrap().set("Perms", dictionary! {}); }
                "extraction-denied" => {
                    let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner", user_password: "", key_length: 128, permissions: lopdf::Permissions::empty() };
                    pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
                }
                _ => {
                    let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner", user_password: "", key_length: 128, permissions: lopdf::Permissions::all() };
                    pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
                }
            }
            let path = folder.path().join(format!("{kind}.pdf")); pdf.save(&path).unwrap();
            let opened = call(&service, |reply| Request::BeginOpen(path, reply)).unwrap();
            let info = match opened {
                OpenResult::Opened { document, .. } => document,
                OpenResult::PasswordRequired { request_id, .. } => match call(&service, |reply| Request::Unlock(request_id, String::new(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{kind} did not unlock") },
            };
            let error = ocr_raster(&service, crate::ocr::OcrPageRequest { document_id: info.id, revision: 0, page: 0 }, crate::ocr_process::OcrCancellation::default()).unwrap_err();
            match kind {
                "signed" => assert!(error.contains("Signed or certified")),
                "extraction-denied" => assert!(error.contains("does not allow")),
                _ => assert!(error.contains("encrypted or restricted")),
            }
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
        call(&service, |reply| Request::Close(ordinary.id, reply)).unwrap();
        assert!(ocr_raster(&service, crate::ocr::OcrPageRequest { document_id: ordinary.id, revision: 0, page: 0 }, crate::ocr_process::OcrCancellation::default()).unwrap_err().contains("closed"));
    }

    #[test]
    fn ocr_worker_holds_admission_until_aborted_raster_and_validation_requests_drain() {
        let _ocr_guard = crate::ocr_process::test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let request = crate::ocr::OcrPageRequest { document_id: info.id, revision: 0, page: 0 };

        for validation in [false, true] {
            let (release, gate) = mpsc::channel();
            let (ready, waiting) = oneshot::channel();
            service.sender.send(Request::BacklogGate(gate, ready)).unwrap();
            waiting.blocking_recv().unwrap().unwrap();
            let cancellation = crate::ocr_process::OcrCancellation::default();
            let operation = crate::ocr_process::OcrOperation::try_begin(&cancellation).unwrap();
            if validation {
                let (reply, receiver) = oneshot::channel(); drop(receiver);
                service.sender.send(Request::ValidateOcrResult(request, operation.worker_hold(), reply)).unwrap();
            } else {
                let (reply, receiver) = oneshot::channel(); drop(receiver);
                service.sender.send(Request::OcrRaster(request, cancellation.clone(), operation.worker_hold(), reply)).unwrap();
            }
            cancellation.cancel();
            drop(operation);
            assert!(matches!(crate::ocr_process::OcrOperation::try_begin(&crate::ocr_process::OcrCancellation::default()), Err(crate::ocr_process::OcrProcessError::Busy)));
            release.send(Vec::new()).unwrap();
            let work = call(&service, |reply| Request::OcrWorkForDocument(info.id, reply)).unwrap();
            assert_eq!((work.dequeued, work.preflighted, work.rendered, work.converted), if validation { (0, 0, 0, 0) } else { (1, 0, 0, 0) });
            let next = crate::ocr_process::OcrOperation::try_begin(&crate::ocr_process::OcrCancellation::default()).unwrap();
            next.finish(Ok(())).unwrap();
        }
        call(&service, |reply| Request::Close(info.id, reply)).unwrap();
    }

    #[test]
    fn raster_redaction_renders_all_pages_removes_selected_pixels_and_preserves_source() {
        let _print_guard = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source_path = root.join("resources/welcome.pdf");
        let source_bytes = std::fs::read(&source_path).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let info = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap();
        let rectangles = vec![crate::raster_redaction::RasterRedactionRect { x: 0.0, y: 0.0, width: 72.0, height: 72.0 }];
        let preflight = call(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision, 0, rectangles.clone(), reply)).unwrap();
        assert_eq!(preflight.suggested_name, "welcome-redacted.pdf");
        assert!(call(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision + 1, 0, rectangles.clone(), reply)).unwrap_err().contains("changed"));
        assert!(call(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision, 6, rectangles.clone(), reply)).is_err());
        assert!(call(&service, |reply| Request::PreflightRasterRedaction(info.id, info.revision, 0, Vec::new(), reply)).is_err());

        let output = folder.path().join("redacted.pdf");
        let saved = call(&service, |reply| Request::RasterRedact(info.id, info.revision, 0, rectangles.clone(), output.clone(), reply)).unwrap();
        assert_eq!(saved.document.pages.len(), info.pages.len());
        assert_eq!(PathBuf::from(&saved.path), output);
        let output_bytes = std::fs::read(&output).unwrap();
        crate::sanitization::verify_clean_raster_pdf(&output_bytes, info.pages.len()).unwrap();
        let parsed = lopdf::Document::load_mem(&output_bytes).unwrap();
        for page_number in 1..=info.pages.len() as u32 { assert_eq!(parsed.extract_text(&[page_number]).unwrap(), ""); }

        let page_ids = parsed.get_pages();
        let image_rgb = |page_number: u32| {
            let page = parsed.get_dictionary(page_ids[&page_number]).unwrap();
            let image_id = page.get(b"Resources").unwrap().as_dict().unwrap().get(b"XObject").unwrap().as_dict().unwrap().get(b"Image").unwrap().as_reference().unwrap();
            let image = parsed.get_object(image_id).unwrap().as_stream().unwrap();
            let width = image.dict.get(b"Width").unwrap().as_i64().unwrap() as u32;
            let height = image.dict.get(b"Height").unwrap().as_i64().unwrap() as u32;
            (width, height, image.decompressed_content().unwrap())
        };
        let (width, height, first_rgb) = image_rgb(1);
        let source_rgb = |physical_page: u16, path: PathBuf| {
            let receipt = call(&service, |reply| Request::ExportPageImage(info.id, info.revision, physical_page, 150, PageImageFormat::Png, path.clone(), reply)).unwrap();
            let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
            let mut reader = decoder.read_info().unwrap();
            let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
            let frame = reader.next_frame(&mut rgb).unwrap();
            rgb.truncate(frame.buffer_size());
            (receipt.width, receipt.height, rgb)
        };
        let (first_width, first_height, first_source_rgb) = source_rgb(0, folder.path().join("redaction-source-page-1.png"));
        assert_eq!((first_width, first_height), (width, height));
        let black_width = (72.0f64 * f64::from(width) / f64::from(info.pages[0].width)).ceil() as usize;
        let black_height = (72.0f64 * f64::from(height) / f64::from(info.pages[0].height)).ceil() as usize;
        for y in 0..height as usize {
            for x in 0..width as usize {
                let offset = (y * width as usize + x) * 3;
                if x < black_width && y < black_height { assert_eq!(&first_rgb[offset..offset + 3], &[0, 0, 0]); }
                else { assert_eq!(&first_rgb[offset..offset + 3], &first_source_rgb[offset..offset + 3]); }
            }
        }
        let (second_width, second_height, second_rgb) = image_rgb(2);
        let (source_width, source_height, second_source_rgb) = source_rgb(1, folder.path().join("redaction-source-page-2.png"));
        assert_eq!((source_width, source_height), (second_width, second_height));
        assert_eq!(second_rgb, second_source_rgb, "unselected pages must retain their complete rendered pixels");
        assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        let existing = output_bytes.clone();
        assert!(call(&service, |reply| Request::RasterRedact(info.id, info.revision, 0, rectangles, output.clone(), reply)).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), existing);
        call(&service, |reply| Request::Close(saved.document.id, reply)).unwrap();
        call(&service, |reply| Request::Close(info.id, reply)).unwrap();
    }
    #[test]
    fn bounded_viewer_backlog_reports_overflow_supersession_and_drains_dropped_receivers(){
        let(sender,receiver,state)=admission_sender();let mut admitted=Vec::new();for width in 64..96{let(tx,rx)=oneshot::channel();sender.send(Request::Render(1,0,width,tx)).unwrap();admitted.push(Some(rx));}
        let(replacement_tx,replacement_rx)=oneshot::channel();sender.send(Request::Render(1,0,64,replacement_tx)).unwrap();assert!(admitted[0].take().unwrap().blocking_recv().unwrap().unwrap_err().contains("superseded"));for width in 96..104{let(tx,rx)=oneshot::channel();sender.send(Request::Render(1,0,width,tx)).unwrap();assert!(rx.blocking_recv().unwrap().unwrap_err().contains("queue is full"));}for value in admitted.iter_mut().skip(1).take(12){drop(value.take());}
        let(close_tx,_)=oneshot::channel();sender.send(Request::Close(1,close_tx)).unwrap();let mut markers=0;for request in receiver.try_iter(){if let Request::AdmittedRender(_,_,_,token)=request{markers+=1;let reply={let mut a=state.lock().unwrap();let key=a.keys.iter().find_map(|(key,value)|(*value==token).then_some(*key)).unwrap();a.keys.remove(&key);a.replies.remove(&token)};if let Some(reply)=reply{if !reply.is_closed(){let _=reply.send(Err("Document is closed".into()));}}}}assert_eq!(markers,32);assert_eq!(state.lock().unwrap().keys.len(),0);assert!(replacement_rx.blocking_recv().unwrap().unwrap_err().contains("closed"));for receiver in admitted.into_iter().flatten(){assert!(receiver.blocking_recv().unwrap().unwrap_err().contains("closed"));}
    }
    fn admission_sender()->(RequestSender,mpsc::Receiver<Request>,Arc<Mutex<RenderAdmission>>){queue_sender(WORKER_QUEUE_LIMIT)}
    fn queue_sender(queue_limit:usize)->(RequestSender,mpsc::Receiver<Request>,Arc<Mutex<RenderAdmission>>){let(inner,receiver)=mpsc::channel();let renders=Arc::new(Mutex::new(RenderAdmission::default()));(RequestSender{inner,renders:renders.clone(),limit:32,pending:Arc::default(),queue_limit},receiver,renders)}
    fn pending(sender:&RequestSender)->usize{sender.pending.load(std::sync::atomic::Ordering::Acquire)}
    #[test]
    fn worker_queue_rejects_excess_non_render_requests_but_never_renders_or_cleanup(){
        let(sender,receiver,state)=queue_sender(4);
        for page in 0..4{sender.send(Request::Text(1,page,0,oneshot::channel().0)).unwrap();}
        let error=sender.send(Request::Text(1,4,0,oneshot::channel().0)).unwrap_err();assert!(matches!(error,SendRejected::Busy));assert!(error.to_string().contains("busy"));
        assert!(matches!(sender.send(Request::Bookmarks(1,0,oneshot::channel().0)),Err(SendRejected::Busy)));
        assert_eq!(pending(&sender),4,"a rejected request must not hold a slot");
        sender.send(Request::Render(1,0,64,oneshot::channel().0)).unwrap();assert_eq!(state.lock().unwrap().keys.len(),1,"viewer renders keep their own admission");
        sender.send(Request::Close(1,oneshot::channel().0)).unwrap();sender.send(Request::CancelPassword(2,oneshot::channel().0)).unwrap();sender.send(Request::EndPrint(3,oneshot::channel().0)).unwrap();sender.send_cleanup(Request::Close(4,oneshot::channel().0)).unwrap();
        assert_eq!(pending(&sender),8,"cleanup is counted but never refused");
        let mut queue=RequestQueue{pending:sender.pending.clone(),..RequestQueue::default()};
        let mut texts=0;for _ in 0..9{if let Request::Text(..)=queue.next(&receiver).unwrap(){texts+=1;}}
        assert_eq!(texts,4);assert_eq!(pending(&sender),0,"every dequeued request frees its slot");assert!(receiver.try_recv().is_err());
        for page in 0..4{sender.send(Request::TextGeometry(1,page,0,oneshot::channel().0)).unwrap();}
        assert!(matches!(sender.send(Request::Properties(1,0,oneshot::channel().0)),Err(SendRejected::Busy)));
        drop(receiver);assert!(matches!(sender.send(Request::Close(1,oneshot::channel().0)),Err(SendRejected::Closed)));assert_eq!(pending(&sender),4,"a send on a closed channel returns its slot");
    }
    #[test]
    fn worker_queue_limit_admits_the_largest_ui_fan_out_with_nothing_drained(){
        let(sender,receiver,_)=queue_sender(WORKER_QUEUE_LIMIT);
        for id in 1..=4u64{
            sender.send(Request::Bookmarks(id,0,oneshot::channel().0)).unwrap();sender.send(Request::PageLabels(id,0,oneshot::channel().0)).unwrap();sender.send(Request::Properties(id,0,oneshot::channel().0)).unwrap();sender.send(Request::FormFields(id,0,oneshot::channel().0)).unwrap();sender.send(Request::Annotations(id,0,oneshot::channel().0)).unwrap();sender.send(Request::Comments(id,0,oneshot::channel().0)).unwrap();
        }
        for page in 0..64{sender.send(Request::TextGeometry(1,page,0,oneshot::channel().0)).unwrap();}
        sender.send(Request::Text(1,0,0,oneshot::channel().0)).unwrap();sender.send(Request::CheckpointRecovery(PathBuf::new(),1,0,0,oneshot::channel().0)).unwrap();sender.send(Request::Edit(1,PageEdit::Undo,oneshot::channel().0)).unwrap();
        let queued=pending(&sender);assert_eq!(queued,4*6+64+3);assert!(queued*2<=WORKER_QUEUE_LIMIT,"the worst UI fan-out keeps at least half the queue free");
        assert_eq!(receiver.try_iter().count(),queued);
    }
    #[test]
    fn unchanged_recovery_checkpoint_skips_the_journal_write(){
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let source_path=folder.path().join("source.pdf");let source=std::fs::read(manifest.join("resources/welcome.pdf")).unwrap();std::fs::write(&source_path,&source).unwrap();let identity=recovery_source_identity(&source).unwrap();
        let service=PdfService::start(manifest.join("resources/pdfium/bin/pdfium.dll"));let opened=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();
        let journal=|revision:u64|{let record=crate::recovery_store::read_recovery(&recovery,identity,None,revision).unwrap().unwrap();(record.generation,record.current_page)};
        let edited=call(&service,|reply|Request::Edit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).unwrap();
        call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,0,reply)).unwrap();assert_eq!(journal(edited.revision),(1,0));
        call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,0,reply)).unwrap();assert_eq!(journal(edited.revision),(1,0),"an unchanged checkpoint must not rewrite the journal");
        std::fs::write(&source_path,b"changed").unwrap();assert!(call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,0,reply)).unwrap_err().contains("changed outside"),"a skipped write still verifies the source");std::fs::write(&source_path,&source).unwrap();
        call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,1,reply)).unwrap();assert_eq!(journal(edited.revision),(2,1),"a new current page is a change");
        call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,1,reply)).unwrap();assert_eq!(journal(edited.revision),(2,1));
        let edited=call(&service,|reply|Request::Edit(edited.id,PageEdit::Rotate{pages:vec![1],clockwise:true},reply)).unwrap();
        call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,1,reply)).unwrap();assert_eq!(journal(edited.revision),(3,1),"a new revision is a change");
        call(&service,|reply|Request::Close(edited.id,reply)).unwrap();
    }
    #[test]
    fn viewer_render_admission_bounds_coalesces_and_respects_barriers(){
        let(sender,receiver,state)=admission_sender();let mut replies=Vec::new();
        for width in 64..96{let(tx,rx)=oneshot::channel();sender.send(Request::Render(1,0,width,tx)).unwrap();replies.push(rx)}
        assert_eq!(state.lock().unwrap().keys.len(),32);
        let(tx,overflow)=oneshot::channel();sender.send(Request::Render(1,0,96,tx)).unwrap();assert!(overflow.blocking_recv().unwrap().unwrap_err().contains("queue is full"));
        let(old_tx,old_rx)=oneshot::channel();let(sender2,receiver2,state2)=admission_sender();sender2.send(Request::Render(7,2,300,old_tx)).unwrap();let(new_tx,_new_rx)=oneshot::channel();sender2.send(Request::Render(7,2,300,new_tx)).unwrap();assert!(old_rx.blocking_recv().unwrap().unwrap_err().contains("superseded"));assert_eq!(receiver2.try_iter().count(),1);assert_eq!(state2.lock().unwrap().keys.len(),1);
        let(close_tx,_)=oneshot::channel();sender2.send(Request::Close(99,close_tx)).unwrap();let(after_tx,_)=oneshot::channel();sender2.send(Request::Render(7,2,300,after_tx)).unwrap();assert_eq!(receiver2.try_iter().count(),2,"a non-render barrier must split coalescing epochs");assert_eq!(state2.lock().unwrap().keys.len(),2);
        drop(replies);assert_eq!(receiver.try_iter().filter(|r|matches!(r,Request::AdmittedRender(..))).count(),32);
    }
    #[test]
    #[ignore = "uses an isolated recovery-root worker and must run alone because PDFium binding is process-global"]
    fn acknowledged_edits_are_verified_and_undo_to_clean_publishes_tombstone() {
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let source_path=folder.path().join("source.pdf");let source=std::fs::read(manifest.join("resources/welcome.pdf")).unwrap();std::fs::write(&source_path,&source).unwrap();
        let service=PdfService::start_worker(manifest.join("resources/pdfium/bin/pdfium.dll"),4096,Some(recovery.clone()));let opened=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();
        let edited=call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},2,reply)).unwrap();let identity=recovery_source_identity(&source).unwrap();let active=crate::recovery_store::read_recovery(&recovery,identity,None,edited.revision).unwrap().unwrap();assert!(active.active);assert_eq!(active.current_page,2);assert_eq!(active.edit_state.pages[0].turns,1);let saved=call(&service,|reply|Request::Save(opened.id,None,folder.path().join("copy.pdf"),reply)).unwrap();assert!(saved.document.dirty);assert_eq!(crate::recovery_store::read_recovery(&recovery,identity,None,edited.revision).unwrap().unwrap(),active);let journal=std::fs::read_dir(&recovery).unwrap().map(|entry|entry.unwrap().path()).find(|path|path.extension().and_then(|value|value.to_str())==Some("smacrec")).unwrap();let mut replaced=active.clone();replaced.current_page=1;std::fs::write(&journal,crate::recovery_journal::encode_recovery(&replaced).unwrap()).unwrap();assert!(call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),opened.id,edited.revision,2,reply)).err().unwrap().contains("replaced"));std::fs::write(&journal,crate::recovery_journal::encode_recovery(&active).unwrap()).unwrap();
        let clean=call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Undo,2,reply)).unwrap();assert!(!clean.dirty&&clean.can_redo);let tombstone=crate::recovery_store::read_recovery(&recovery,identity,None,clean.revision).unwrap().unwrap();assert!(!tombstone.active);assert!(tombstone.generation>active.generation);assert!(tombstone.edit_state.pages.is_empty());
        std::fs::write(&journal,crate::recovery_journal::encode_recovery(&active).unwrap()).unwrap();let rollback=call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Redo,0,reply)).err().unwrap();assert!(rollback.contains("moved backward"));let state=call(&service,|reply|Request::OcrWorkerState(opened.id,reply)).unwrap();assert!(!state.dirty&&state.can_redo);assert_eq!(state.revision,clean.revision);std::fs::write(&journal,crate::recovery_journal::encode_recovery(&tombstone).unwrap()).unwrap();
        call(&service,|reply|Request::Close(opened.id,reply)).unwrap();let(tx,rx)=oneshot::channel();service.sender.send(Request::RestoreRecovery(recovery,source_path,tx)).unwrap();assert!(rx.blocking_recv().unwrap().unwrap().is_none());
    }

    #[test]
    #[ignore = "uses an isolated recovery-root worker and must run alone because PDFium binding is process-global"]
    fn journal_failure_and_external_source_change_roll_back_exact_session() {
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let source_path=folder.path().join("source.pdf");let source=std::fs::read(manifest.join("resources/welcome.pdf")).unwrap();std::fs::write(&source_path,&source).unwrap();
        let service=PdfService::start_worker(manifest.join("resources/pdfium/bin/pdfium.dll"),4096,Some(recovery.clone()));let opened=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();assert!(call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},u32::MAX,reply)).err().unwrap().contains("Current page"));std::fs::write(&source_path,b"changed").unwrap();assert!(call(&service,|reply|Request::Edit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).err().unwrap().contains("changed outside"));assert!(call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),opened.id,opened.revision,0,reply)).err().unwrap().contains("unsaved"));std::fs::write(&source_path,&source).unwrap();std::fs::rename(&recovery,folder.path().join("recovery-backup")).unwrap();std::fs::write(&recovery,b"not-a-directory").unwrap();assert!(call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).is_err());let state=call(&service,|reply|Request::OcrWorkerState(opened.id,reply)).unwrap();assert_eq!(state.revision,0);assert!(!state.dirty&&!state.can_undo&&!state.can_redo);assert_eq!(state.plan[0].turns,0);
    }

    #[test]
    #[ignore = "uses an isolated recovery-root worker and must run alone because PDFium binding is process-global"]
    fn explicit_discard_is_idempotent_and_resolves_post_publish_lock() {
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let source_path=folder.path().join("source.pdf");let source=std::fs::read(manifest.join("resources/welcome.pdf")).unwrap();std::fs::write(&source_path,&source).unwrap();
        let service=PdfService::start_worker(manifest.join("resources/pdfium/bin/pdfium.dll"),4096,Some(recovery.clone()));let opened=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let edited=call(&service,|reply|Request::DurableEdit(opened.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).unwrap();call(&service,|reply|Request::DiscardRecovery(edited.id,edited.revision,reply)).unwrap();let identity=recovery_source_identity(&source).unwrap();let tombstone=crate::recovery_store::read_recovery(&recovery,identity,None,0).unwrap().unwrap();assert!(!tombstone.active&&tombstone.generation==2);std::fs::write(&source_path,b"changed").unwrap();call(&service,|reply|Request::DiscardRecovery(edited.id,edited.revision,reply)).unwrap();std::fs::write(&source_path,&source).unwrap();
        let renewed=call(&service,|reply|Request::DurableEdit(edited.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).unwrap();let active=crate::recovery_store::read_recovery(&recovery,identity,None,renewed.revision).unwrap().unwrap();assert!(active.active&&active.generation==3);assert!(call(&service,|reply|Request::DiscardRecovery(renewed.id,edited.revision,reply)).is_err());crate::recovery_store::fail_next_verification_after_publish();let error=call(&service,|reply|Request::DurableEdit(renewed.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).err().unwrap();assert!(error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED));assert!(call(&service,|reply|Request::DurableEdit(renewed.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).err().unwrap().starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED));call(&service,|reply|Request::Close(renewed.id,reply)).unwrap();let reopened=call(&service,|reply|Request::BeginOpen(source_path.clone(),reply)).unwrap().accept();let clean=match reopened{OpenResult::Opened{document,recovery:Some(_)}=>document,_=>panic!("committed edit must be offered after locked close")};let kept=call(&service,|reply|Request::KeepRecoveredEdits(clean.id,clean.revision,reply)).unwrap();let requested=kept.document.revision;crate::recovery_store::fail_next_verification_after_publish();let error=call(&service,|reply|Request::DurableEdit(clean.id,PageEdit::Rotate{pages:vec![0],clockwise:true},0,reply)).err().unwrap();assert!(error.starts_with(RECOVERY_COMMITTED_REOPEN_REQUIRED));call(&service,|reply|Request::DiscardRecovery(clean.id,requested,reply)).expect("locked discard");call(&service,|reply|Request::DiscardRecovery(clean.id,requested,reply)).expect("idempotent locked discard");let inactive=crate::recovery_store::read_recovery(&recovery,identity,None,0).unwrap().unwrap();assert!(!inactive.active&&inactive.generation==5);assert_eq!(std::fs::read(&source_path).unwrap(),source);call(&service,|reply|Request::Close(clean.id,reply)).unwrap();
    }

    #[test]
    #[ignore = "uses an isolated recovery-root worker and must run alone because PDFium binding is process-global"]
    fn normal_open_offers_active_recovery_and_explicit_choices_are_monotonic() {
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let source_path=folder.path().join("source.pdf");std::fs::copy(manifest.join("resources/welcome.pdf"),&source_path).unwrap();let service=PdfService::start_worker(manifest.join("resources/pdfium/bin/pdfium.dll"),4096,Some(recovery.clone()));
        let first=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let shadow=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let edited=call(&service,|reply|Request::Edit(first.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).unwrap();assert!(call(&service,|reply|Request::Edit(shadow.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).is_err());call(&service,|reply|Request::Close(edited.id,reply)).unwrap();assert!(call(&service,|reply|Request::Edit(shadow.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).is_err());
        let opened=call(&service,|reply|Request::BeginOpen(source_path.clone(),reply)).unwrap().accept();let (clean,offer)=match opened{OpenResult::Opened{document,recovery}=>(document,recovery.unwrap()),_=>panic!("ordinary source must open")};assert_eq!(clean.revision,0);assert_eq!(offer.revision,edited.revision);
        let kept=call(&service,|reply|Request::KeepRecoveredEdits(clean.id,clean.revision,reply)).unwrap();assert!(kept.document.dirty);assert_eq!(kept.document.revision,edited.revision);call(&service,|reply|Request::Close(clean.id,reply)).unwrap();
        let opened=call(&service,|reply|Request::BeginOpen(source_path.clone(),reply)).unwrap().accept();let clean=match opened{OpenResult::Opened{document,recovery:Some(_)}=>document,_=>panic!("active recovery must be offered")};let original=call(&service,|reply|Request::OpenOriginal(clean.id,clean.revision,reply)).unwrap();assert!(!original.dirty);let identity=recovery_source_identity(&std::fs::read(source_path).unwrap()).unwrap();let record=crate::recovery_store::read_recovery(&recovery,identity,None,0).unwrap().unwrap();assert!(!record.active);assert_eq!(record.generation,2);assert!(call(&service,|reply|Request::OpenOriginal(clean.id,clean.revision,reply)).is_err());
    }

    #[test]
    #[ignore = "uses an isolated recovery-root worker and must run alone because PDFium binding is process-global"]
    fn recovery_checkpoint_ack_is_durable_and_explicit_restore_replays_exact_state(){
        let manifest=PathBuf::from(env!("CARGO_MANIFEST_DIR"));let folder=tempfile::tempdir().unwrap();let recovery=folder.path().join("recovery");std::fs::create_dir(&recovery).unwrap();let service=PdfService::start_worker(manifest.join("resources/pdfium/bin/pdfium.dll"),4096,Some(recovery.clone()));let source_path=folder.path().join("source.pdf");let source=std::fs::read(manifest.join("resources/welcome.pdf")).unwrap();std::fs::write(&source_path,&source).unwrap();
        let opened=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();assert!(call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),opened.id,opened.revision,0,reply)).unwrap_err().contains("unsaved"));
        let edited=call(&service,|reply|Request::Edit(opened.id,PageEdit::Move{from:5,to:0},reply)).unwrap();let edited=call(&service,|reply|Request::Edit(edited.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).unwrap();let edited=call(&service,|reply|Request::Crop(edited.id,0,edited.revision,CropRect{x:0.05,y:0.1,width:0.8,height:0.75},reply)).unwrap();let edited=call(&service,|reply|Request::Comment(edited.id,edited.revision,CommentMutation::CreateHighlight(0,CropRect{x:0.1,y:0.1,width:0.2,height:0.2},Some("Recovered highlight".into())),reply)).unwrap();let expected=call(&service,|reply|Request::Render(edited.id,0,240,reply)).unwrap();
        std::fs::write(&source_path,b"changed").unwrap();assert!(call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,2,reply)).err().unwrap().contains("changed outside"));std::fs::write(&source_path,&source).unwrap();call(&service,|reply|Request::CheckpointRecovery(recovery.clone(),edited.id,edited.revision,2,reply)).unwrap();let identity=recovery_source_identity(&source).unwrap();let durable=crate::recovery_store::read_recovery(&recovery,identity,None,edited.revision).unwrap().unwrap();assert_eq!((durable.generation,durable.revision,durable.current_page),(5,edited.revision,2));assert_eq!(std::fs::read(&source_path).unwrap(),source);
        call(&service,|reply|Request::Close(edited.id,reply)).unwrap();let(tx,rx)=oneshot::channel();service.sender.send(Request::RestoreRecovery(recovery.clone(),source_path.clone(),tx)).unwrap();let restored=rx.blocking_recv().unwrap().unwrap().unwrap().accept();assert_eq!(restored.current_page,2);assert_eq!(restored.document.revision,edited.revision);assert!(restored.document.dirty&&restored.document.can_undo&&!restored.document.can_redo);assert_eq!(call(&service,|reply|Request::Render(restored.document.id,0,240,reply)).unwrap(),expected);let annotations=call(&service,|reply|Request::Annotations(restored.document.id,restored.document.revision,reply)).unwrap();assert_eq!(annotations.annotations.len(),1);assert_eq!(annotations.annotations[0].contents.as_deref(),Some("Recovered highlight"));let advanced=call(&service,|reply|Request::DurableEdit(restored.document.id,PageEdit::Rotate{pages:vec![0],clockwise:true},2,reply)).unwrap();let durable=crate::recovery_store::read_recovery(&recovery,identity,None,advanced.revision).unwrap().unwrap();assert_eq!(durable.generation,6);assert_eq!(durable.current_page,2);
        let missing=folder.path().join("missing");std::fs::create_dir(&missing).unwrap();let(tx,rx)=oneshot::channel();service.sender.send(Request::RestoreRecovery(missing,source_path.clone(),tx)).unwrap();assert!(rx.blocking_recv().unwrap().unwrap().is_none());call(&service,|reply|Request::Close(restored.document.id,reply)).unwrap();assert_eq!(std::fs::read(source_path).unwrap(),source);
    }
    #[test]
    fn searchable_ocr_publication_reopens_pixels_and_exact_pdfium_text() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap();
        let page = crate::searchable_pdf::SearchableRasterPage {
            page_width: 100.0, page_height: 100.0, image_width: 10, image_height: 10,
            rgb: vec![255; 300],
            words: vec![crate::searchable_pdf::SearchableWord { x: 10.0, y: 10.0, width: 50.0, height: 12.0, text: "Hello".into() }],
        };
        let bytes = crate::searchable_pdf::write_searchable_pdf(&[page.clone()]).unwrap();
        let source_path = folder.path().join("source.pdf");
        std::fs::write(&source_path, &bytes).unwrap();
        let source = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap();
        let preflight = call(&service, |reply| Request::PreflightSearchableOcr(source.id, source.revision, reply)).unwrap();
        assert_eq!(preflight.page_count, 1);
        let output_path = folder.path().join("searchable.pdf");
        let prepared = crate::searchable_ocr_job::PreparedSearchableOcr {
            bytes: bytes.clone(), pages: vec![page.clone()], expected_text: vec!["Hello".into()],
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30),
            cancellation: crate::ocr_process::OcrCancellation::default(),
        };
        let saved = call(&service, |reply| Request::PublishSearchableOcr(source.id, source.revision, output_path.clone(), prepared, reply)).unwrap();
        assert_eq!(std::fs::read(&output_path).unwrap(), bytes);
        assert_eq!(std::fs::read(&source_path).unwrap(), bytes);
        assert_eq!(saved.document.revision, 0);
        assert!(!saved.document.dirty);
        let cancelled_path = folder.path().join("cancelled.pdf");
        let cancellation = crate::ocr_process::OcrCancellation::default();
        assert!(cancellation.cancel());
        let cancelled = crate::searchable_ocr_job::PreparedSearchableOcr {
            bytes: bytes.clone(), pages: vec![page.clone()], expected_text: vec!["Hello".into()],
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(30), cancellation,
        };
        assert!(call(&service, |reply| Request::PublishSearchableOcr(source.id, source.revision, cancelled_path.clone(), cancelled, reply)).err().unwrap().contains("cancelled"));
        assert!(!cancelled_path.exists());
        let expired_path = folder.path().join("expired.pdf");
        let expired = crate::searchable_ocr_job::PreparedSearchableOcr {
            bytes, pages: vec![page], expected_text: vec!["Hello".into()],
            deadline: std::time::Instant::now() - std::time::Duration::from_secs(1),
            cancellation: crate::ocr_process::OcrCancellation::default(),
        };
        assert!(call(&service, |reply| Request::PublishSearchableOcr(source.id, source.revision, expired_path.clone(), expired, reply)).err().unwrap().contains("time limit"));
        assert!(!expired_path.exists());
        for id in [source.id, saved.document.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
    }

    #[test]
    fn multi_image_pdf_reopens_every_page_and_preserves_existing_session() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let open = call(&service, |reply| Request::Open(root.join("resources/welcome.pdf"), reply)).unwrap();
        let fixture_root = root.join("tests/fixtures/searchable-ocr");
        let sources = ["single-line.png", "two-column.png", "mixed-lines.png"]
            .map(|name| std::fs::read(fixture_root.join(name)).unwrap()).to_vec();
        let options = crate::image_pdf::ImagePdfOptions {
            page_size: crate::image_pdf::ImagePdfPageSize::A4,
            orientation: crate::image_pdf::ImagePdfOrientation::Auto,
            margin_points: 12.0,
        };
        let prepared = crate::image_pdf::prepare_many_bytes(sources.clone(), options).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let output = folder.path().join("images.pdf");
        let saved = call(&service, |reply| Request::CreateMultiImagePdf(prepared, output.clone(), reply)).unwrap();
        assert_eq!(saved.document.pages.len(), 3);
        assert_eq!(std::fs::read(&output).unwrap(), std::fs::read(&saved.path).unwrap());
        let rendered = call(&service, |reply| Request::Render(saved.document.id, 2, 96, reply)).unwrap();
        assert!(!rendered.is_empty());
        let still_open = call(&service, |reply| Request::Properties(open.id, open.revision, reply)).unwrap();
        assert_eq!(still_open.page_count, open.pages.len());
        let second = crate::image_pdf::prepare_many_bytes(sources, options).unwrap();
        assert!(call(&service, |reply| Request::CreateMultiImagePdf(second, output.clone(), reply)).err().unwrap().contains("never overwrites"));
        for id in [saved.document.id, open.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
    }

    #[test]
    fn strict_image_replacement_publishes_new_copy_and_preserves_source_session() {
        use lopdf::{content::{Content, Operation}, dictionary, Dictionary, Document, Object, Stream};
        let mut pdf = Document::with_version("1.7");
        let pages = pdf.new_object_id();
        let mut image = Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 2, "Height" => 1, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, vec![255, 0, 0, 0, 255, 0]);
        image.compress().unwrap();
        let image_id = pdf.add_object(image);
        let content = Content { operations: vec![Operation::new("q", vec![]), Operation::new("cm", vec![200.into(), 0.into(), 0.into(), 100.into(), 0.into(), 0.into()]), Operation::new("Do", vec![Object::Name(b"Image".to_vec())]), Operation::new("Q", vec![])] }.encode().unwrap();
        let contents = pdf.add_object(Stream::new(Dictionary::new(), content));
        let page_id = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()], "Resources" => dictionary! { "XObject" => dictionary! { "Image" => image_id } }, "Contents" => contents });
        let mut image_two = Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => 1, "Height" => 2, "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8 }, vec![10, 20, 30, 40, 50, 60]);
        image_two.compress().unwrap();
        let image_two_id = pdf.add_object(image_two);
        let content_two = Content { operations: vec![Operation::new("q", vec![]), Operation::new("cm", vec![100.into(), 0.into(), 0.into(), 200.into(), 0.into(), 0.into()]), Operation::new("Do", vec![Object::Name(b"Image".to_vec())]), Operation::new("Q", vec![])] }.encode().unwrap();
        let contents_two = pdf.add_object(Stream::new(Dictionary::new(), content_two));
        let page_two_id = pdf.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 100.into(), 200.into()], "Resources" => dictionary! { "XObject" => dictionary! { "Image" => image_two_id } }, "Contents" => contents_two });
        pdf.objects.insert(pages, dictionary! { "Type" => "Pages", "Count" => 2, "Kids" => vec![Object::Reference(page_id), Object::Reference(page_two_id)] }.into());
        let catalog = pdf.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages }); pdf.trailer.set("Root", catalog);
        let mut source_bytes = Vec::new(); pdf.save_to(&mut source_bytes).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let folder = tempfile::tempdir().unwrap(); let source_path = folder.path().join("source.pdf"); std::fs::write(&source_path, &source_bytes).unwrap();
        let source = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap();
        let deleted = call(&service, |reply| Request::Edit(source.id, PageEdit::Delete { pages: vec![1] }, reply)).unwrap();
        assert!(call(&service, |reply| Request::InspectImageReplacement(source.id, deleted.revision, 0, reply)).is_err());
        let restored = call(&service, |reply| Request::Edit(source.id, PageEdit::Undo, reply)).unwrap();
        assert!(restored.can_redo);
        let target = call(&service, |reply| Request::InspectImageReplacement(source.id, restored.revision, 0, reply)).unwrap();
        assert_eq!((target.pixel_width, target.pixel_height), (2, 1));
        let output_path = folder.path().join("replaced.pdf");
        let replacement = vec![0, 0, 255, 255, 255, 255];
        let saved = call(&service, |reply| Request::ReplaceImageCopy(source.id, restored.revision, 0, target, replacement.clone(), output_path.clone(), reply)).unwrap();
        assert_eq!(std::fs::read(&source_path).unwrap(), source_bytes);
        let redone = call(&service, |reply| Request::Edit(source.id, PageEdit::Redo, reply)).unwrap();
        assert!(redone.dirty); assert!(redone.can_undo); assert_eq!(redone.pages.len(), 1);
        let output = lopdf::Document::load(&output_path).unwrap();
        assert_eq!(saved.document.pages.len(), 2);
        assert_eq!(output.get_object(image_id).unwrap().as_stream().unwrap().decompressed_content().unwrap(), replacement);
        assert_eq!(saved.document.revision, 0); assert!(!saved.document.dirty);
        for id in [source.id, saved.document.id] { call(&service, |reply| Request::Close(id, reply)).unwrap(); }
    }

    #[test]
    fn strict_text_replacement_requires_pdfium_geometry_and_preserves_source_session(){
        use lopdf::{content::{Content,Operation},dictionary,Dictionary,Document,Object,Stream};let mut pdf=Document::with_version("1.7");let pages=pdf.new_object_id();let font=pdf.add_object(dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});let content=Content{operations:vec![Operation::new("BT",vec![]),Operation::new("Tf",vec![Object::Name(b"F1".to_vec()),24.into()]),Operation::new("Tm",vec![1.into(),0.into(),0.into(),1.into(),40.into(),80.into()]),Operation::new("Tj",vec![Object::string_literal("123")]),Operation::new("ET",vec![])]}.encode().unwrap();let stream=pdf.add_object(Stream::new(Dictionary::new(),content));let page=pdf.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),200.into(),120.into()],"Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>font}},"Contents"=>stream});pdf.objects.insert(pages,dictionary!{"Type"=>"Pages","Count"=>1,"Kids"=>vec![Object::Reference(page)]}.into());let root=pdf.add_object(dictionary!{"Type"=>"Catalog","Pages"=>pages});pdf.trailer.set("Root",root);pdf.reference_table.cross_reference_type=lopdf::xref::XrefType::CrossReferenceTable;pdf.trailer.remove(b"Type");let mut source_bytes=Vec::new();pdf.save_to(&mut source_bytes).unwrap();
        let service=PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"));let folder=tempfile::tempdir().unwrap();let source_path=folder.path().join("text-source.pdf");std::fs::write(&source_path,&source_bytes).unwrap();let source=call(&service,|reply|Request::Open(source_path.clone(),reply)).unwrap();let rotated=call(&service,|reply|Request::Edit(source.id,PageEdit::Rotate{pages:vec![0],clockwise:true},reply)).unwrap();assert!(call(&service,|reply|Request::InspectTextReplacement(source.id,rotated.revision,0,reply)).is_err());let restored=call(&service,|reply|Request::Edit(source.id,PageEdit::Undo,reply)).unwrap();assert!(restored.can_redo);let target=call(&service,|reply|Request::InspectTextReplacement(source.id,restored.revision,0,reply)).unwrap();assert_eq!(target.target.text,"123");let output=folder.path().join("text-copy.pdf");let saved=call(&service,|reply|Request::ReplaceTextCopy(source.id,restored.revision,0,target,"789".into(),output.clone(),reply)).unwrap();assert_eq!(crate::text_edit::inspect_text_run(&std::fs::read(&output).unwrap(),1).unwrap().text,"789");assert_eq!(std::fs::read(&source_path).unwrap(),source_bytes);let redone=call(&service,|reply|Request::Edit(source.id,PageEdit::Redo,reply)).unwrap();assert!(redone.dirty&&redone.can_undo);assert_eq!(saved.document.revision,0);for id in[source.id,saved.document.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}
    }

    #[test]
    fn searchable_publication_commit_arbitrates_cancel_and_expiry() {
        let folder = tempfile::tempdir().unwrap();
        let cancelled_path = folder.path().join("cancel-before-commit.pdf");
        let cancelled = crate::ocr_process::OcrCancellation::default();
        let cancel_at_boundary = || { assert!(cancelled.cancel()); };
        assert!(publish_searchable_file(
            &cancelled_path, b"candidate", std::time::Instant::now() + std::time::Duration::from_secs(30),
            &cancelled, Some(&cancel_at_boundary), None,
        ).unwrap_err().contains("cancelled"));
        assert!(!cancelled_path.exists());

        let committed_path = folder.path().join("commit-wins.pdf");
        let committed = crate::ocr_process::OcrCancellation::default();
        let cancel_after_commit = || { assert!(!committed.cancel()); assert!(committed.is_completed()); };
        publish_searchable_file(
            &committed_path, b"candidate", std::time::Instant::now() + std::time::Duration::from_secs(30),
            &committed, None, Some(&cancel_after_commit),
        ).unwrap();
        assert_eq!(std::fs::read(&committed_path).unwrap(), b"candidate");

        let expired_path = folder.path().join("expired-during-staging.pdf");
        let expired = crate::ocr_process::OcrCancellation::default();
        let expire_at_boundary = || { std::thread::sleep(std::time::Duration::from_millis(2)); };
        assert!(publish_searchable_file(
            &expired_path, b"candidate", std::time::Instant::now() + std::time::Duration::from_millis(1),
            &expired, Some(&expire_at_boundary), None,
        ).unwrap_err().contains("time limit"));
        assert!(!expired_path.exists());
        assert!(!expired.is_completed());
    }

    #[test]
    fn raster_redaction_preflight_refuses_protected_and_aggregate_oversize_sources() {
        use lopdf::dictionary;
        let _print_guard = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let rectangles = vec![crate::raster_redaction::RasterRedactionRect { x: 0.0, y: 0.0, width: 1.0, height: 1.0 }];
        let large = call(&service, |reply| Request::Open(root.join("../test-corpus/synthetic-scan-98.pdf"), reply)).unwrap();
        assert!(call(&service, |reply| Request::PreflightRasterRedaction(large.id, 0, 0, rectangles.clone(), reply)).unwrap_err().contains("aggregate"));
        call(&service, |reply| Request::Close(large.id, reply)).unwrap();
        for kind in ["certified", "signature"] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            if kind == "certified" { pdf.catalog_mut().unwrap().set("Perms", dictionary! {}); }
            else { pdf.add_object(dictionary! { "Type" => "Sig" }); }
            let path = folder.path().join(format!("redaction-{kind}.pdf")); pdf.save(&path).unwrap();
            let protected = call(&service, |reply| Request::Open(path, reply)).unwrap();
            assert!(call(&service, |reply| Request::PreflightRasterRedaction(protected.id, 0, 0, rectangles.clone(), reply)).unwrap_err().contains("Signed or certified"));
            call(&service, |reply| Request::Close(protected.id, reply)).unwrap();
        }
        for (kind, user_password, permissions) in [("encrypted", "reader", lopdf::Permissions::all()), ("restricted", "", lopdf::Permissions::empty())] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            pdf.trailer.set("ID", vec![lopdf::Object::string_literal(format!("redaction-{kind}")), lopdf::Object::string_literal(format!("redaction-{kind}"))]);
            let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner", user_password, key_length: 128, permissions };
            pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
            let path = folder.path().join(format!("redaction-{kind}.pdf")); pdf.save(&path).unwrap();
            let opened = call(&service, |reply| Request::BeginOpen(path, reply)).unwrap();
            let protected = match opened {
                OpenResult::Opened { document, .. } => document,
                OpenResult::PasswordRequired { request_id, .. } => match call(&service, |reply| Request::Unlock(request_id, user_password.to_owned(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{kind} did not unlock") },
            };
            assert!(call(&service, |reply| Request::PreflightRasterRedaction(protected.id, 0, 0, rectangles.clone(), reply)).unwrap_err().contains("encrypted or restricted"));
            call(&service, |reply| Request::Close(protected.id, reply)).unwrap();
        }
    }

    #[test]
    fn page_image_export_tracks_current_plan_pixels_and_preserves_session() {
        let _print_guard = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let source_path = root.join("resources/welcome.pdf");
        let source_bytes = std::fs::read(&source_path).unwrap();
        let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let mut info = call(&service, |reply| Request::Open(source_path.clone(), reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 5, to: 0 }, reply)).unwrap();
        info = call(&service, |reply| Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, reply)).unwrap();
        info = call(&service, |reply| Request::Crop(info.id, 0, info.revision, CropRect { x: 0.0, y: 0.0, width: 1.0, height: 0.5 }, reply)).unwrap();
        info = call(&service, |reply| Request::Comment(info.id, info.revision, CommentMutation::CreateHighlight(0, CropRect { x: 0.15, y: 0.15, width: 0.4, height: 0.25 }, Some("PNG pixel proof".into())), reply)).unwrap();
        let revision = info.revision;
        let preview = call(&service, |reply| Request::Render(info.id, 0, 173, reply)).unwrap();
        let preflight = call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, 72, PageImageFormat::Png, reply)).unwrap();
        assert_eq!(preflight.suggested_name, "welcome-page-1.png");
        let jpeg_preflight = call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, reply)).unwrap();
        assert_eq!(jpeg_preflight.suggested_name, "welcome-page-1.jpg");
        for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
            for dpi in [72, 150, 300] { assert!(call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, dpi, format, reply)).is_ok()); }
            for dpi in [0, 73, 600] { assert!(call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, dpi, format, reply)).is_err()); }
            assert!(call(&service, |reply| Request::PreflightPageImage(info.id, revision - 1, 0, 72, format, reply)).is_err());
            assert!(call(&service, |reply| Request::PreflightPageImage(info.id, revision, 6, 72, format, reply)).is_err());
        }

        let output = folder.path().join("current.png");
        let receipt = call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, output.clone(), reply)).unwrap();
        assert_eq!((receipt.document_id, receipt.revision, receipt.page, receipt.dpi, receipt.width, receipt.height), (info.id, revision, 0, 72, 792, 306));
        assert_eq!(receipt.format, PageImageFormat::Png);
        assert_eq!(PathBuf::from(&receipt.path), output);
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&output).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().pixel_dims.unwrap().xppu, 2_835);
        let mut rgb = vec![0; reader.output_buffer_size().unwrap()];
        let png_info = reader.next_frame(&mut rgb).unwrap();
        assert_eq!((png_info.width, png_info.height, png_info.color_type, png_info.bit_depth), (receipt.width, receipt.height, png::ColorType::Rgb, png::BitDepth::Eight));
        let snapshot = call(&service, |reply| Request::BeginPrint(info.id, revision, reply)).unwrap();
        let print = call(&service, |reply| Request::PrintRender(snapshot.token, 0, receipt.width, receipt.height, reply)).unwrap();
        assert_eq!((print.width, print.height), (receipt.width, receipt.height));
        let expected = print.bgra.chunks_exact(4).flat_map(|pixel| {
            let alpha = u16::from(pixel[3]);
            [pixel[2], pixel[1], pixel[0]].map(|channel| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8)
        }).collect::<Vec<_>>();
        assert_eq!(rgb, expected, "PNG pixels must match an independent current print-quality render");

        let jpeg_output = folder.path().join("current.jpg");
        let jpeg_receipt = call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, jpeg_output.clone(), reply)).unwrap();
        assert_eq!((jpeg_receipt.document_id, jpeg_receipt.revision, jpeg_receipt.page, jpeg_receipt.dpi, jpeg_receipt.width, jpeg_receipt.height, jpeg_receipt.format), (info.id, revision, 0, 72, receipt.width, receipt.height, PageImageFormat::Jpeg));
        assert_eq!(PathBuf::from(&jpeg_receipt.path), jpeg_output);
        assert_eq!(serde_json::to_value(&jpeg_receipt).unwrap()["format"], "jpeg");
        let jpeg_bytes = std::fs::read(&jpeg_output).unwrap();
        assert!(jpeg_bytes.starts_with(&[0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10]));
        assert_eq!((jpeg_bytes[13], u16::from_be_bytes([jpeg_bytes[14], jpeg_bytes[15]]), u16::from_be_bytes([jpeg_bytes[16], jpeg_bytes[17]])), (1, 72, 72));
        let jpeg_decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(std::fs::File::open(&jpeg_output).unwrap())).unwrap();
        assert_eq!(jpeg_decoder.dimensions(), (receipt.width, receipt.height));
        assert_eq!(jpeg_decoder.color_type(), image::ColorType::Rgb8);
        let mut jpeg_rgb = vec![0; jpeg_decoder.total_bytes() as usize];
        jpeg_decoder.read_image(&mut jpeg_rgb).unwrap();
        let differences = jpeg_rgb.iter().zip(&rgb).map(|(jpeg, png)| jpeg.abs_diff(*png)).collect::<Vec<_>>();
        let total_difference = differences.iter().map(|difference| u64::from(*difference)).sum::<u64>();
        let mean_difference_milli = total_difference * 1_000 / differences.len() as u64;
        let max_difference = differences.into_iter().max().unwrap();
        assert!(total_difference <= rgb.len() as u64 * 12, "quality-90 JPEG mean channel difference exceeded 12");
        assert!(max_difference <= 128, "quality-90 JPEG maximum channel difference was {max_difference}");
        assert_ne!(jpeg_rgb, rgb, "JPEG output must remain explicitly lossy");
        let jpeg_probe = root.join("../target/page-image-probe/current-edited-highlight-q90-72dpi.jpg");
        if !jpeg_probe.exists() { std::fs::copy(&jpeg_output, &jpeg_probe).unwrap(); }
        println!("PAGE_IMAGE_JPEG_PROBE path={} format=jpeg quality=90 density=72dpi dimensions={}x{} encoded_bytes={} mean_channel_difference_milli={} max_channel_difference={}", jpeg_probe.display(), jpeg_receipt.width, jpeg_receipt.height, jpeg_bytes.len(), mean_difference_milli, max_difference);
        assert_eq!(call(&service, |reply| Request::Render(info.id, 0, 173, reply)).unwrap(), preview);
        let render_work = call(&service, |reply| Request::RenderWorkForDocument(info.id, reply)).unwrap(); assert_eq!(render_work.rendered, 1, "page-image export must not disturb the existing viewer render cache");
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(output.clone(), reply)).unwrap().is_empty(), "PNG export must not register an output session");
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(jpeg_output.clone(), reply)).unwrap().is_empty(), "JPEG export must not register an output session");

        let dropped_after_publish = folder.path().join("published-before-ipc-drop.png");
        let (published_reply, published_receiver) = oneshot::channel(); service.sender.send(Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, dropped_after_publish.clone(), published_reply)).unwrap();
        call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, 72, PageImageFormat::Png, reply)).unwrap();
        assert!(!published_receiver.is_empty() && dropped_after_publish.exists()); drop(published_receiver);
        assert!(dropped_after_publish.exists(), "A successfully published PNG is user-owned after the IPC receiver drops");
        assert!(call(&service, |reply| Request::OpenDocumentsForPath(dropped_after_publish.clone(), reply)).unwrap().is_empty());

        let existing = std::fs::read(&output).unwrap();
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, output.clone(), reply)).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), existing);
        let existing_jpeg = std::fs::read(&jpeg_output).unwrap();
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, jpeg_output.clone(), reply)).is_err());
        assert_eq!(std::fs::read(&jpeg_output).unwrap(), existing_jpeg);
        let wrong = folder.path().join("wrong.jpg");
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, wrong.clone(), reply)).is_err()); assert!(!wrong.exists());
        let wrong_jpeg = folder.path().join("wrong-jpeg.png");
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, wrong_jpeg.clone(), reply)).is_err()); assert!(!wrong_jpeg.exists());
        let stale = folder.path().join("stale.png");
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision - 1, 0, 72, PageImageFormat::Png, stale.clone(), reply)).is_err()); assert!(!stale.exists());
        let stale_jpeg = folder.path().join("stale.jpg");
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision - 1, 0, 72, PageImageFormat::Jpeg, stale_jpeg.clone(), reply)).is_err()); assert!(!stale_jpeg.exists());
        let canceled = folder.path().join("closed-reply.png");
        let (reply, receiver) = oneshot::channel(); drop(receiver); service.sender.send(Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, canceled.clone(), reply)).unwrap();
        let stable = call(&service, |reply| Request::Edit(info.id, PageEdit::Move { from: 0, to: 0 }, reply)).unwrap();
        assert!(!canceled.exists());
        let canceled_jpeg = folder.path().join("closed-reply.jpg");
        let (jpeg_reply, jpeg_receiver) = oneshot::channel(); drop(jpeg_receiver); service.sender.send(Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, canceled_jpeg.clone(), jpeg_reply)).unwrap();
        call(&service, |reply| Request::PreflightPageImage(info.id, revision, 0, 72, PageImageFormat::Jpeg, reply)).unwrap();
        assert!(!canceled_jpeg.exists());
        assert_eq!(stable.revision, revision); assert!(stable.dirty && stable.can_undo && !stable.can_redo);
        assert_eq!(std::fs::read(&source_path).unwrap(), source_bytes);
        call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        let closed = folder.path().join("closed.png");
        assert!(call(&service, |reply| Request::ExportPageImage(info.id, revision, 0, 72, PageImageFormat::Png, closed.clone(), reply)).is_err()); assert!(!closed.exists());
        let probe = root.join("../target/page-image-probe/current-edited-highlight-72dpi.png"); std::fs::create_dir_all(probe.parent().unwrap()).unwrap(); if !probe.exists() { std::fs::copy(&output, &probe).unwrap(); }
        println!("PAGE_IMAGE_SERVICE_PROBE path={} document={} revision={} page=0 dpi=72 dimensions={}x{}", probe.display(), info.id, revision, receipt.width, receipt.height);
    }

    #[test]
    fn page_image_security_and_representative_fixture_pages_are_checked() {
        use lopdf::dictionary;
        let _print_guard = print_test_lock();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")); let folder = tempfile::tempdir().unwrap();
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        for (fixture, count) in [("resources/welcome.pdf", 6usize), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let info = call(&service, |reply| Request::Open(root.join(fixture), reply)).unwrap();
            for page in [0usize, count / 2, count - 1] {
                let path = folder.path().join(format!("fixture-{count}-page-{}.png", page + 1));
                let receipt = call(&service, |reply| Request::ExportPageImage(info.id, 0, page as u16, 72, PageImageFormat::Png, path.clone(), reply)).unwrap();
                assert_eq!(receipt.page, page as u16); assert!(receipt.width > 0 && receipt.height > 0 && path.exists());
            }
            call(&service, |reply| Request::Close(info.id, reply)).unwrap();
        }
        for kind in ["certified", "signature", "byte-range"] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            match kind {
                "certified" => { pdf.catalog_mut().unwrap().set("Perms", dictionary! {}); },
                "signature" => { pdf.add_object(dictionary! { "Type" => "Sig" }); },
                _ => { pdf.add_object(dictionary! { "ByteRange" => vec![0.into(), 1.into(), 2.into(), 3.into()] }); },
            }
            let source = folder.path().join(format!("{kind}.pdf")); pdf.save(&source).unwrap();
            let info = call(&service, |reply| Request::Open(source, reply)).unwrap();
            for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
                assert!(call(&service, |reply| Request::PreflightPageImage(info.id, 0, 0, 72, format, reply)).unwrap_err().contains("Signed or certified"));
                let output = folder.path().join(format!("{kind}.{}", format.extension())); assert!(call(&service, |reply| Request::ExportPageImage(info.id, 0, 0, 72, format, output.clone(), reply)).is_err()); assert!(!output.exists());
            }
        }

        for (kind, user_password, permissions) in [("encrypted", "test password", lopdf::Permissions::all()), ("restricted", "", lopdf::Permissions::empty())] {
            let mut pdf = lopdf::Document::load(root.join("resources/welcome.pdf")).unwrap();
            pdf.trailer.set("ID", vec![lopdf::Object::string_literal(format!("png-{kind}")), lopdf::Object::string_literal(format!("png-{kind}"))]);
            let encryption = lopdf::EncryptionVersion::V2 { document: &pdf, owner_password: "owner password", user_password, key_length: 128, permissions };
            pdf.encrypt(&lopdf::EncryptionState::try_from(encryption).unwrap()).unwrap();
            let source = folder.path().join(format!("{kind}.pdf")); pdf.save(&source).unwrap();
            let opened = call(&service, |reply| Request::BeginOpen(source, reply)).unwrap();
            let info = match opened {
                OpenResult::Opened { document, .. } => document,
                OpenResult::PasswordRequired { request_id, .. } => match call(&service, |reply| Request::Unlock(request_id, user_password.to_owned(), reply)).unwrap() { OpenResult::Opened { document, .. } => document, _ => panic!("{kind} did not unlock") },
            };
            for format in [PageImageFormat::Png, PageImageFormat::Jpeg] {
                assert!(call(&service, |reply| Request::PreflightPageImage(info.id, 0, 0, 72, format, reply)).is_err(), "{kind} {format:?}");
                let output = folder.path().join(format!("{kind}.{}", format.extension())); assert!(call(&service, |reply| Request::ExportPageImage(info.id, 0, 0, 72, format, output.clone(), reply)).is_err(), "{kind} {format:?}"); assert!(!output.exists());
            }
        }

        let form = call(&service, |reply| Request::Open(root.join("tests/fixtures/reportlab-choice-fields.pdf"), reply)).unwrap();
        let fields = call(&service, |reply| Request::FormFields(form.id, 0, reply)).unwrap(); assert_eq!(fields.status, "supported"); assert_eq!(fields.fields.len(), 2);
        let form_png = folder.path().join("form-appearance.png");
        let receipt = call(&service, |reply| Request::ExportPageImage(form.id, 0, 0, 72, PageImageFormat::Png, form_png.clone(), reply)).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&form_png).unwrap())); let mut reader = decoder.read_info().unwrap();
        let mut rgb = vec![0; reader.output_buffer_size().unwrap()]; reader.next_frame(&mut rgb).unwrap();
        let snapshot = call(&service, |reply| Request::BeginPrint(form.id, 0, reply)).unwrap();
        let print = call(&service, |reply| Request::PrintRender(snapshot.token, 0, receipt.width, receipt.height, reply)).unwrap();
        let expected = print.bgra.chunks_exact(4).flat_map(|pixel| { let alpha = u16::from(pixel[3]); [pixel[2], pixel[1], pixel[0]].map(|channel| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8) }).collect::<Vec<_>>();
        assert_eq!(rgb, expected, "PNG must retain the same form appearances as print rendering");
        assert!(rgb.chunks_exact(3).filter(|pixel| pixel.iter().any(|channel| *channel < 245)).count() > 1_000, "form fixture must contain independently visible appearance pixels");
        let form_jpeg = folder.path().join("form-appearance.jpg");
        let jpeg_receipt = call(&service, |reply| Request::ExportPageImage(form.id, 0, 0, 72, PageImageFormat::Jpeg, form_jpeg.clone(), reply)).unwrap();
        assert_eq!((jpeg_receipt.width, jpeg_receipt.height, jpeg_receipt.format), (receipt.width, receipt.height, PageImageFormat::Jpeg));
        let decoder = image::codecs::jpeg::JpegDecoder::new(std::io::BufReader::new(std::fs::File::open(form_jpeg).unwrap())).unwrap();
        let mut jpeg = vec![0; decoder.total_bytes() as usize]; decoder.read_image(&mut jpeg).unwrap();
        let differences = jpeg.iter().zip(&expected).map(|(actual, expected)| actual.abs_diff(*expected)).collect::<Vec<_>>();
        let total_difference = differences.iter().map(|difference| u64::from(*difference)).sum::<u64>();
        let max_difference = differences.into_iter().max().unwrap();
        assert!(total_difference <= jpeg.len() as u64 * 12, "form JPEG mean channel difference exceeded 12");
        assert!(max_difference <= 160, "form JPEG maximum channel difference was {max_difference}");
    }
    #[test]
    fn cache_evicts_and_clears_closed_documents() {
        let mut cache = Cache { entries: VecDeque::new(), weight: 0 };
        cache.insert((1, 0, 800), vec![1], 300 * 1024 * 1024);
        cache.insert((2, 0, 800), vec![2], 300 * 1024 * 1024);
        assert!(cache.get((1, 0, 800)).is_none()); assert_eq!(cache.get((2, 0, 800)), Some(vec![2]));
        cache.close(2); assert_eq!(cache.weight, 0);
    }
    #[test]
    fn renders_scan_corpus_and_rejects_invalid_page() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let (tx, rx) = oneshot::channel();
        service.sender.send(Request::Open(root.join("../test-corpus/synthetic-scan-98.pdf"), tx)).unwrap();
        let info = rx.blocking_recv().unwrap().unwrap(); assert_eq!(info.pages.len(), 98);
        for page in 0..98 {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, page, 816, tx)).unwrap();
            let bytes = rx.blocking_recv().unwrap().unwrap(); assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 98, 816, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Close(info.id, tx)).unwrap(); rx.blocking_recv().unwrap().unwrap();
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 816, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
        for (path, count) in [("resources/welcome.pdf", 6), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(root.join(path), tx)).unwrap();
            let info = rx.blocking_recv().unwrap().unwrap(); assert_eq!(info.pages.len(), count);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, (count - 1) as u16, 816, tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_ok());
        }
        let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(root.join("../test-corpus/invalid.pdf"), tx)).unwrap(); assert!(rx.blocking_recv().unwrap().is_err());
    }

    #[test]
    fn edited_preview_matches_reopened_copy_for_every_valid_fixture() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let service = PdfService::start(root.join("resources/pdfium/bin/pdfium.dll"));
        let output = tempfile::tempdir().unwrap();
        for (fixture, count) in [("resources/welcome.pdf", 6), ("../test-corpus/synthetic-scan-98.pdf", 98), ("../test-corpus/synthetic-text-1500.pdf", 1500)] {
            let source_path = root.join(fixture);
            let source_bytes = std::fs::read(&source_path).unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(source_path.clone(), tx)).unwrap();
            let info = rx.blocking_recv().unwrap().unwrap();
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Edit(info.id, PageEdit::Rotate { pages: vec![0], clockwise: true }, tx)).unwrap();
            let changed = rx.blocking_recv().unwrap().unwrap();
            assert!(changed.dirty); assert!(changed.can_undo); assert_eq!(changed.pages[0].width, 792.0);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(info.id, 0, 260, tx)).unwrap();
            let preview = image::load_from_memory(&rx.blocking_recv().unwrap().unwrap()).unwrap().into_rgba8();
            let path = output.path().join(format!("organized-{count}.pdf"));
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Save(info.id, None, path.clone(), tx)).unwrap();
            assert!(rx.blocking_recv().unwrap().unwrap().document.dirty);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Open(path, tx)).unwrap();
            let reopened = rx.blocking_recv().unwrap().unwrap(); assert_eq!(reopened.pages.len(), count);
            let (tx, rx) = oneshot::channel(); service.sender.send(Request::Render(reopened.id, 0, 260, tx)).unwrap();
            let saved = image::load_from_memory(&rx.blocking_recv().unwrap().unwrap()).unwrap().into_rgba8();
            assert_eq!(preview.dimensions(), saved.dimensions());
            assert!(preview.as_raw() == saved.as_raw(), "Preview differs from saved copy for {fixture}");
            assert_eq!(std::fs::read(source_path).unwrap(), source_bytes);
        }
    }

    #[test]
    #[ignore = "writes controlled native outputs only when SMACROBAT_CROSS_VIEWER_OUTPUT_DIR names an existing empty target directory"]
    fn export_controlled_cross_viewer_outputs() {
        use lopdf::{content::{Content, Operation}, dictionary, Dictionary, Document, Object, Stream};
        use std::{fs::OpenOptions, io::Write as _, path::Path};

        fn sha256(path: &Path) -> String { format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())) }
        fn create_new(path: &Path, bytes: &[u8]) {
            let mut file = OpenOptions::new().write(true).create_new(true).open(path).unwrap();
            file.write_all(bytes).unwrap(); file.sync_all().unwrap();
        }
        fn render_rgb(service: &PdfService, id: u64, page: u16, width: u32) -> image::RgbImage {
            image::load_from_memory(&call(service, |reply| Request::Render(id, page, width as i32, reply)).unwrap()).unwrap().into_rgb8()
        }
        fn sample(image: &image::RgbImage, region: [u32; 4]) -> serde_json::Value {
            let [left, top, right, bottom] = region;
            assert!(left < right && top < bottom && right <= image.width() && bottom <= image.height());
            let mut total = [0u64; 3]; let count = u64::from((right - left) * (bottom - top));
            for y in top..bottom { for x in left..right { for channel in 0..3 { total[channel] += u64::from(image.get_pixel(x, y)[channel]); } } }
            let mean = total.map(|value| ((value + count / 2) / count) as i64);
            serde_json::json!({"name":"feature-region native-PDFium reference","region":region,"minimum":mean.map(|value|(value-48).max(0)),"maximum":mean.map(|value|(value+48).min(255))})
        }
        fn darkest_region(image: &image::RgbImage) -> [u32; 4] {
            let (x,y,_)=image.enumerate_pixels().min_by_key(|(_,_,pixel)|u16::from(pixel[0])+u16::from(pixel[1])+u16::from(pixel[2])).unwrap();
            let left=x.saturating_sub(6).min(image.width()-12); let top=y.saturating_sub(6).min(image.height()-12); [left,top,left+12,top+12]
        }
        fn case(
            id: &str, profile: &str, root: &Path, source: &Path, output: &Path, page: u16,
            points: [f32; 2], rendered: &image::RgbImage, region: [u32; 4],
        ) -> serde_json::Value {
            serde_json::json!({
                "id":id,"profile":profile,"source":source.file_name().unwrap().to_str().unwrap(),
                "sourceSha256":sha256(source),"output":output.file_name().unwrap().to_str().unwrap(),
                "outputSha256":sha256(output),"page":u32::from(page)+1,"dpi":72,
                "pagePoints":[points[0],points[1]],"nativePixelSize":[rendered.width(),rendered.height()],
                "pixelSize":[points[0].ceil() as u32,points[1].ceil() as u32],
                "samples":[sample(rendered,region)],"directory":root.file_name().unwrap().to_str().unwrap(),
            })
        }
        fn text_pdf(text: &str) -> Vec<u8> {
            let mut pdf = Document::with_version("1.7"); let pages = pdf.new_object_id();
            let font = pdf.add_object(dictionary!{"Type"=>"Font","Subtype"=>"Type1","BaseFont"=>"Helvetica","Encoding"=>"WinAnsiEncoding"});
            let content = Content { operations: vec![Operation::new("BT",vec![]),Operation::new("Tf",vec![Object::Name(b"F1".to_vec()),24.into()]),Operation::new("Tm",vec![1.into(),0.into(),0.into(),1.into(),40.into(),80.into()]),Operation::new("Tj",vec![Object::string_literal(text)]),Operation::new("ET",vec![])] }.encode().unwrap();
            let stream = pdf.add_object(Stream::new(Dictionary::new(),content));
            let page = pdf.add_object(dictionary!{"Type"=>"Page","Parent"=>pages,"MediaBox"=>vec![0.into(),0.into(),200.into(),120.into()],"Resources"=>dictionary!{"Font"=>dictionary!{"F1"=>font}},"Contents"=>stream});
            pdf.objects.insert(pages,dictionary!{"Type"=>"Pages","Count"=>1,"Kids"=>vec![Object::Reference(page)]}.into());
            let catalog=pdf.add_object(dictionary!{"Type"=>"Catalog","Pages"=>pages}); pdf.trailer.set("Root",catalog);
            pdf.reference_table.cross_reference_type=lopdf::xref::XrefType::CrossReferenceTable; pdf.trailer.remove(b"Type");
            let mut bytes=Vec::new(); pdf.save_to(&mut bytes).unwrap(); bytes
        }

        let requested = PathBuf::from(std::env::var_os("SMACROBAT_CROSS_VIEWER_OUTPUT_DIR").expect("set SMACROBAT_CROSS_VIEWER_OUTPUT_DIR"));
        let root = std::fs::canonicalize(&requested).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
        let target = std::fs::canonicalize(repo.join("target")).unwrap();
        assert!(root.starts_with(&target) && root != target, "output must be an owned directory below target");
        for ancestor in root.ancestors().take_while(|path| path.starts_with(&target)) {
            let metadata = std::fs::symlink_metadata(ancestor).unwrap(); assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
            #[cfg(windows)] { use std::os::windows::fs::MetadataExt; assert_eq!(metadata.file_attributes() & 0x400, 0, "output path contains a reparse point"); }
        }
        assert!(std::fs::read_dir(&root).unwrap().next().is_none(), "output directory must be empty");

        let service = PdfService::start(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll"));
        let mut cases = Vec::new();

        let comments_source = root.join("comments-source.pdf"); let comments_bytes = text_pdf("ABC"); create_new(&comments_source,&comments_bytes);
        let mut comments = call(&service,|reply|Request::Open(comments_source.clone(),reply)).unwrap();
        comments=call(&service,|reply|Request::Comment(comments.id,comments.revision,CommentMutation::Create(0,CropRect{x:0.05,y:0.05,width:0.12,height:0.12},"Controlled note".into()),reply)).unwrap();
        comments=call(&service,|reply|Request::Comment(comments.id,comments.revision,CommentMutation::CreateHighlight(0,CropRect{x:0.20,y:0.20,width:0.25,height:0.18},Some("Controlled area".into())),reply)).unwrap();
        comments=call(&service,|reply|Request::Comment(comments.id,comments.revision,CommentMutation::CreateTextHighlight(0,0,3,Some("Controlled text".into())),reply)).unwrap();
        let comments_output=root.join("comments-output.pdf"); comments=call(&service,|reply|Request::Save(comments.id,None,comments_output.clone(),reply)).unwrap().document;
        let annotations=call(&service,|reply|Request::Annotations(comments.id,comments.revision,reply)).unwrap();
        assert_eq!(annotations.annotations.iter().filter(|value|value.kind==crate::comments::AnnotationKind::Highlight).count(),2);
        assert_eq!(call(&service,|reply|Request::Comments(comments.id,comments.revision,reply)).unwrap().notes.len(),1);
        assert_eq!(std::fs::read(&comments_source).unwrap(),comments_bytes);
        let image=render_rgb(&service,comments.id,0,comments.pages[0].width.ceil() as u32);
        cases.push(case("native-comments","comments",&root,&comments_source,&comments_output,0,[comments.pages[0].width,comments.pages[0].height],&image,[45,28,85,50]));
        call(&service,|reply|Request::Close(comments.id,reply)).unwrap();

        let forms_source=root.join("forms-source.pdf"); let forms_bytes=include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf"); create_new(&forms_source,forms_bytes);
        let forms=call(&service,|reply|Request::Open(forms_source.clone(),reply)).unwrap(); let fields=call(&service,|reply|Request::FormFields(forms.id,0,reply)).unwrap();
        let patches=fields.fields.iter().map(|field|if field.checked.is_some(){crate::forms::FieldValue::Checkbox{field_id:field.field_id.clone(),checked:true}}else{crate::forms::FieldValue::Text{field_id:field.field_id.clone(),value:"Controlled form".into()}}).collect();
        let forms_output=root.join("forms-output.pdf"); let forms_saved=call(&service,|reply|Request::FillFormCopy(forms.id,0,patches,forms_output.clone(),reply)).unwrap();
        let filled=call(&service,|reply|Request::FormFields(forms_saved.document.id,0,reply)).unwrap(); assert!(filled.fields.iter().any(|field|field.checked==Some(true))); assert!(filled.fields.iter().any(|field|field.value=="Controlled form"));
        assert_eq!(std::fs::read(&forms_source).unwrap(),forms_bytes); let image=render_rgb(&service,forms_saved.document.id,0,forms_saved.document.pages[0].width.ceil() as u32);
        cases.push(case("native-forms","forms",&root,&forms_source,&forms_output,0,[forms_saved.document.pages[0].width,forms_saved.document.pages[0].height],&image,[58,114,82,138]));
        for id in [forms.id,forms_saved.document.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}

        let redaction_source=root.join("redaction-source.pdf"); let redaction_bytes=include_bytes!("../resources/welcome.pdf"); create_new(&redaction_source,redaction_bytes);
        let redaction=call(&service,|reply|Request::Open(redaction_source.clone(),reply)).unwrap(); let rectangles=vec![crate::raster_redaction::RasterRedactionRect{x:0.0,y:0.0,width:72.0,height:72.0}];
        let redaction_output=root.join("redaction-output.pdf"); let redacted=call(&service,|reply|Request::RasterRedact(redaction.id,0,0,rectangles,redaction_output.clone(),reply)).unwrap();
        let redacted_bytes=std::fs::read(&redaction_output).unwrap(); crate::sanitization::verify_clean_raster_pdf(&redacted_bytes,redaction.pages.len()).unwrap(); assert_eq!(std::fs::read(&redaction_source).unwrap(),redaction_bytes);
        let image=render_rgb(&service,redacted.document.id,0,redacted.document.pages[0].width.ceil() as u32);
        assert!((8..32).all(|y|(8..32).all(|x|image.get_pixel(x,y).0.iter().all(|channel|*channel<8))), "redacted interior must render black in PDFium");
        cases.push(case("native-redaction","raster-copy",&root,&redaction_source,&redaction_output,0,[redacted.document.pages[0].width,redacted.document.pages[0].height],&image,[8,8,32,32]));
        for id in [redaction.id,redacted.document.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}

        let fixture_root=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/searchable-ocr"); let image_sources=["single-line.png","two-column.png","mixed-lines.png"].map(|name|std::fs::read(fixture_root.join(name)).unwrap());
        let mut bundle=Vec::new(); for bytes in &image_sources{bundle.extend_from_slice(&(bytes.len() as u64).to_le_bytes());bundle.extend_from_slice(bytes);} let multi_source=root.join("multiimage-sources.bin"); create_new(&multi_source,&bundle);
        let options=crate::image_pdf::ImagePdfOptions{page_size:crate::image_pdf::ImagePdfPageSize::A4,orientation:crate::image_pdf::ImagePdfOrientation::Auto,margin_points:12.0};
        let prepared=crate::image_pdf::prepare_many_bytes(image_sources.to_vec(),options).unwrap(); let multi_output=root.join("multiimage-output.pdf"); let multi=call(&service,|reply|Request::CreateMultiImagePdf(prepared,multi_output.clone(),reply)).unwrap(); assert_eq!(multi.document.pages.len(),3); assert_eq!(std::fs::read(&multi_source).unwrap(),bundle);
        let image=render_rgb(&service,multi.document.id,2,multi.document.pages[2].width.ceil() as u32); let region=darkest_region(&image); assert!(image.get_pixel((region[0]+region[2])/2,(region[1]+region[3])/2).0.iter().any(|channel|*channel<64));
        cases.push(case("native-multiimage","image-pdf",&root,&multi_source,&multi_output,2,[multi.document.pages[2].width,multi.document.pages[2].height],&image,region)); call(&service,|reply|Request::Close(multi.document.id,reply)).unwrap();

        let text_source=root.join("text-source.pdf"); let text_bytes=text_pdf("123"); create_new(&text_source,&text_bytes); let text=call(&service,|reply|Request::Open(text_source.clone(),reply)).unwrap();
        let target=call(&service,|reply|Request::InspectTextReplacement(text.id,0,0,reply)).unwrap(); let text_output=root.join("text-output.pdf"); let replaced=call(&service,|reply|Request::ReplaceTextCopy(text.id,0,0,target,"789".into(),text_output.clone(),reply)).unwrap();
        assert_eq!(crate::text_edit::inspect_text_run(&std::fs::read(&text_output).unwrap(),1).unwrap().text,"789"); assert_eq!(std::fs::read(&text_source).unwrap(),text_bytes);
        let image=render_rgb(&service,replaced.document.id,0,replaced.document.pages[0].width.ceil() as u32);
        cases.push(case("native-text-replacement","page-operations",&root,&text_source,&text_output,0,[replaced.document.pages[0].width,replaced.document.pages[0].height],&image,[38,20,100,52]));
        for id in [text.id,replaced.document.id]{call(&service,|reply|Request::Close(id,reply)).unwrap();}

        let manifest=serde_json::json!({"schema":1,"scope":"Five exact synthetic native saved outputs; PDFium semantic/pixel checks plus independent Poppler rendering. No Adobe, arbitrary-PDF, or general cross-viewer claim.","cases":cases});
        create_new(&root.join("manifest.json"),format!("{}\n",serde_json::to_string_pretty(&manifest).unwrap()).as_bytes());
    }
}
