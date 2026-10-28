#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod service;
mod pdf_engine;
mod pdf_helper;
mod open_path;
mod open_limits;
mod pdf_load;
mod editor;
mod printing;
mod print_commands;
mod document_properties;
mod text_geometry;
mod text_edit;
mod text_edit_commands;
mod split;
mod combine;
mod comments;
mod forms;
mod page_labels;
mod bookmark_destinations;
mod image_pdf;
#[cfg(windows)]
mod image_edit;
#[cfg(windows)]
mod image_pdf_commands;
#[cfg(windows)]
mod image_edit_commands;
mod page_image;
mod recovery_journal;
mod recovery_store;
mod recovery_commands;
mod sanitization;
mod raster_redaction;
#[cfg(windows)]
mod searchable_ocr;
#[cfg(windows)]
mod searchable_pdf;
#[cfg(windows)]
mod searchable_ocr_job;
mod data_dirs;
mod update_attempt;
mod update_commands;
#[cfg(windows)]
mod notices;
#[cfg(windows)]
#[allow(dead_code)]
mod child_job;
#[cfg(windows)]
#[allow(dead_code)]
mod ocr_process;
#[cfg(windows)]
#[cfg_attr(not(ocr_opt_in), allow(dead_code))]
mod ocr;
#[cfg(windows)]
mod ocr_commands;
#[cfg(all(test, windows))]
#[path = "../ocr_build.rs"]
#[allow(dead_code, unused_imports)]
mod ocr_build;
#[cfg(all(test, windows))]
mod ocr_test_support;
#[cfg(all(test, windows))]
mod viewer_perf;
use service::{DocumentInfo, PdfService};
use tauri::{Manager, State};

/// The system allocator in the app; in the PDF-engine helper, armed to exit cleanly on a refused allocation.
#[cfg(windows)]
#[global_allocator]
static ALLOCATOR: child_job::ModeSwitchAllocator = child_job::ModeSwitchAllocator;

fn update_attempt_root(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    data_dirs::app_data_dir(app).map_err(|error| format!("The update recovery folder is unavailable: {error}"))
}

#[tauri::command]
fn read_update_attempt(app: tauri::AppHandle) -> Result<Option<update_attempt::UpdateAttempt>, String> {
    update_attempt::read(&update_attempt_root(&app)?)
}

#[tauri::command]
fn write_update_attempt(app: tauri::AppHandle, attempt: update_attempt::UpdateAttempt) -> Result<(), String> {
    update_attempt::write(&update_attempt_root(&app)?, &attempt)
}

#[tauri::command]
fn clear_update_attempt(app: tauri::AppHandle) -> Result<(), String> {
    update_attempt::clear(&update_attempt_root(&app)?)
}

#[tauri::command]
async fn open_document(app: tauri::AppHandle, service: State<'_, PdfService>) -> Result<Option<service::OpenResult>, String> {
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).add_filter("PDF documents", &["pdf"]).pick_file()).await.map_err(|e| e.to_string())?;
    match path { Some(path) => service.begin_open_from(path, open_path::OpenOrigin::UserChosen).await.map(Some), None => Ok(None) }
}
#[tauri::command]
async fn reopen_document(service: State<'_, PdfService>, grants: State<'_, open_path::OpenPathGrants>, path: String) -> Result<service::OpenResult, String> {
    let path = std::path::PathBuf::from(path);
    let origin = if grants.consume(&path) { open_path::OpenOrigin::UserChosen } else { open_path::OpenOrigin::Webview };
    service.begin_open_from(path, origin).await
}
#[tauri::command]
async fn unlock_document(service: State<'_, PdfService>, request_id: u64, password: String) -> Result<service::OpenResult, String> { service.unlock(request_id, password).await }
#[tauri::command]
async fn cancel_password_request(service: State<'_, PdfService>, request_id: u64) -> Result<(), String> { service.cancel_password(request_id).await }
#[cfg(windows)]
#[tauri::command]
fn ocr_capability(commands: State<'_, ocr_commands::OcrCommands>) -> ocr_commands::OcrCapability { commands.capability() }
#[cfg(windows)]
#[tauri::command]
async fn recognize_page_ocr(commands: State<'_, ocr_commands::OcrCommands>, request_id: String, id: u64, revision: u64, page: u16) -> Result<ocr_commands::OcrReceipt, String> {
    commands.recognize(request_id, id, revision, page).await
}
#[cfg(windows)]
#[tauri::command]
fn cancel_page_ocr(commands: State<'_, ocr_commands::OcrCommands>, request_id: String) -> Result<ocr_commands::OcrCancelAck, String> { commands.cancel(request_id) }
#[cfg(windows)]
#[tauri::command]
async fn create_searchable_ocr_copy(app: tauri::AppHandle, service: State<'_, PdfService>, commands: State<'_, ocr_commands::OcrCommands>, request_id: String, document_id: u64, revision: u64) -> Result<Option<service::SavedCopy>, String> {
    let preflight = service.preflight_searchable_ocr(document_id, revision).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save a searchable OCR copy").add_filter("PDF documents", &["pdf"]).set_file_name(preflight.suggested_name).save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => commands.create_searchable(request_id, document_id, revision, preflight.page_count, path).await.map(Some), None => Ok(None) }
}
#[tauri::command]
async fn open_example(app: tauri::AppHandle, service: State<'_, PdfService>) -> Result<DocumentInfo, String> {
    service.open(app.path().resource_dir().map_err(|e| e.to_string())?.join("resources/welcome.pdf")).await
}
#[tauri::command]
async fn render_page(service: State<'_, PdfService>, id: u64, page: u16, width: i32) -> Result<tauri::ipc::Response, String> {
    service.render(id, page, width).await.map(tauri::ipc::Response::new)
}
#[tauri::command]
async fn export_page_image(app: tauri::AppHandle, service: State<'_, PdfService>, id: u64, revision: u64, page: u16, dpi: u16, format: Option<page_image::PageImageFormat>) -> Result<Option<service::PageImageReceipt>, String> {
    let format = format.unwrap_or_default();
    let preflight = service.preflight_page_image(id, revision, page, dpi, format).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title(format.dialog_title()).add_filter(format.filter_name(), format.extensions()).set_file_name(preflight.suggested_name).save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.export_page_image(id, revision, page, dpi, format, path).await.map(Some), None => Ok(None) }
}
#[tauri::command]
async fn retry_pdf_engine(service: State<'_, PdfService>, id: u64) -> Result<(), String> { service.retry_engine(id).await }
#[tauri::command]
async fn close_document(service: State<'_, PdfService>, id: u64) -> Result<(), String> { service.close(id).await }

#[tauri::command]
async fn page_text(service: State<'_, PdfService>, id: u64, page: u16, revision: u64) -> Result<String, String> { service.text(id, page, revision).await }

#[tauri::command]
async fn page_text_geometry(service: State<'_, PdfService>, id: u64, page: u16, revision: u64) -> Result<text_geometry::PageTextGeometry, String> { service.text_geometry(id, page, revision).await }

#[tauri::command]
async fn document_bookmarks(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<service::BookmarkList, String> { service.bookmarks(id, revision).await }

#[tauri::command]
async fn document_page_labels(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<page_labels::DocumentPageLabels, String> { service.page_labels(id, revision).await }

#[tauri::command]
async fn document_properties(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<document_properties::DocumentProperties, String> { service.properties(id, revision).await }
#[tauri::command]
async fn document_comments(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<comments::CommentList, String> { service.comments(id, revision).await }
#[tauri::command]
async fn document_annotations(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<comments::AnnotationList, String> { service.annotations(id, revision).await }
#[tauri::command]
async fn create_highlight(service: State<'_, PdfService>, id: u64, revision: u64, page: u16, rect: service::CropRect, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.create_highlight(id, revision, page, rect, contents, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn create_text_highlight(service: State<'_, PdfService>, id: u64, revision: u64, page: u16, start: usize, end: usize, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.create_text_highlight(id, revision, page, start, end, contents, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn update_highlight(service: State<'_, PdfService>, id: u64, revision: u64, annotation_id: String, contents: Option<String>, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.update_highlight(id, revision, annotation_id, contents, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn delete_highlight(service: State<'_, PdfService>, id: u64, revision: u64, annotation_id: String, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.delete_highlight(id, revision, annotation_id, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn create_comment(service: State<'_, PdfService>, id: u64, revision: u64, page: u16, rect: service::CropRect, contents: String, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.create_comment(id, revision, page, rect, contents, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn update_comment(service: State<'_, PdfService>, id: u64, revision: u64, note_id: String, contents: String, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.update_comment(id, revision, note_id, contents, current_page).await.map_err(Into::into) }
#[tauri::command]
async fn delete_comment(service: State<'_, PdfService>, id: u64, revision: u64, note_id: String, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> { service.delete_comment(id, revision, note_id, current_page).await.map_err(Into::into) }

#[tauri::command]
async fn dependency_notices(app: tauri::AppHandle) -> Result<String, String> {
    let resource_dir = app.path().resource_dir().map_err(|e| e.to_string())?;
    #[cfg(windows)]
    return tauri::async_runtime::spawn_blocking(move || notices::load(&resource_dir)).await.map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    {
        let path = resource_dir.join("resources/third-party-licenses/THIRD-PARTY-NOTICES.txt");
        tauri::async_runtime::spawn_blocking(move || std::fs::read_to_string(path).map_err(|e| format!("Could not read bundled notices: {e}"))).await.map_err(|e| e.to_string())?
    }
}

#[tauri::command]
async fn edit_pages(service: State<'_, PdfService>, id: u64, revision: u64, edit: editor::PageEdit, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> {
    service.edit_at(id, revision, edit, current_page).await.map_err(Into::into)
}
#[tauri::command]
async fn crop_page(service: State<'_, PdfService>, id: u64, page: u16, revision: u64, rect: service::CropRect, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> {
    service.crop(id, page, revision, rect, current_page).await.map_err(Into::into)
}
#[tauri::command]
async fn crop_pages(service: State<'_, PdfService>, id: u64, pages: Vec<u16>, revision: u64, insets: service::CropInsets, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> {
    service.crop_pages(id, pages, revision, insets, current_page).await.map_err(Into::into)
}
#[tauri::command]
async fn reset_crops(service: State<'_, PdfService>, id: u64, revision: u64, pages: Vec<u16>, current_page: u32) -> Result<DocumentInfo, recovery_commands::MutationCommandError> {
    service.reset_crops(id, pages, revision, current_page).await.map_err(Into::into)
}
#[tauri::command]
async fn save_copy(app: tauri::AppHandle, service: State<'_, PdfService>, id: u64, revision: u64, pages: Option<Vec<usize>>) -> Result<Option<service::SavedCopy>, String> {
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    service.save_copy_via(id, revision, pages, |suggested| async move {
        tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save a copy as a new file").add_filter("PDF documents", &["pdf"]).set_file_name(suggested).save_file()).await.map_err(|e| e.to_string())
    }).await
}

#[tauri::command]
async fn redact_document(app: tauri::AppHandle, service: State<'_, PdfService>, id: u64, revision: u64, page: u16, rectangles: Vec<raster_redaction::RasterRedactionRect>) -> Result<Option<service::SavedCopy>, String> {
    let preflight = service.preflight_raster_redaction(id, revision, page, rectangles.clone()).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save a redacted raster copy").add_filter("PDF documents", &["pdf"]).set_file_name(preflight.suggested_name).save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.raster_redact(id, revision, page, rectangles, path).await.map(Some), None => Ok(None) }
}

#[tauri::command]
async fn create_pdf_from_image(app: tauri::AppHandle, service: State<'_, PdfService>, options: image_pdf::ImagePdfOptions) -> Result<Option<service::SavedCopy>, String> {
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let source_window = window.clone();
    let source = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&source_window).set_title("Choose one PNG or JPEG image").add_filter("PNG and JPEG images", &["png", "jpg", "jpeg"]).pick_file()).await.map_err(|error| error.to_string())?;
    let Some(source) = source else { return Ok(None); };
    let suggested = format!("{}.pdf", source.file_stem().unwrap_or_default().to_string_lossy());
    let output = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save image as a new PDF").add_filter("PDF documents", &["pdf"]).set_file_name(suggested).save_file()).await.map_err(|error| error.to_string())?;
    match output { Some(output) => service.create_image_pdf(source, output, options).await.map(Some), None => Ok(None) }
}

#[cfg(windows)]
#[tauri::command]
async fn choose_image_pdf_sources(app: tauri::AppHandle, selections: State<'_, image_pdf_commands::ImagePdfSelections>, replace_selection_id: Option<String>) -> Result<Option<image_pdf_commands::ImagePdfSelection>, String> {
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let paths = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Choose PNG or JPEG images").add_filter("PNG and JPEG images", &["png", "jpg", "jpeg"]).pick_files()).await.map_err(|error| error.to_string())?;
    paths.map(|paths| selections.reserve_replacing(paths, replace_selection_id.as_deref())).transpose()
}

#[cfg(windows)]
#[tauri::command]
async fn create_pdf_from_images(app: tauri::AppHandle, service: State<'_, PdfService>, selections: State<'_, image_pdf_commands::ImagePdfSelections>, selection_id: String, source_ids: Vec<String>, options: image_pdf::ImagePdfOptions) -> Result<Option<service::SavedCopy>, String> {
    let (sources, attempt) = selections.begin_attempt(&selection_id, &source_ids)?;
    let result = async {
        let prepared = tauri::async_runtime::spawn_blocking(move || image_pdf::prepare_many_bytes(sources, options)).await.map_err(|error| error.to_string())??;
        let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
        let output = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save images as a new PDF").add_filter("PDF documents", &["pdf"]).set_file_name("images.pdf").save_file()).await.map_err(|error| error.to_string())?;
        match output { Some(output) => { image_pdf_commands::validate_output_path(&output)?; service.create_multi_image_pdf(prepared, output).await.map(Some) }, None => Ok(None) }
    }.await;
    if matches!(&result, Ok(Some(_))) { attempt.commit_success()?; }
    result
}

#[cfg(windows)]
#[tauri::command]
fn cancel_image_pdf_sources(selections: State<'_, image_pdf_commands::ImagePdfSelections>, selection_id: String) -> Result<(), String> { selections.cancel(&selection_id) }

#[cfg(windows)]
#[tauri::command]
async fn inspect_image_replacement_target(service: State<'_, PdfService>, selections: State<'_, image_edit_commands::ImageReplacementSelections>, document_id: u64, revision: u64, page: u16) -> Result<image_edit_commands::ImageReplacementTarget, String> {
    let info = service.inspect_image_replacement(document_id, revision, page).await?;
    selections.register(document_id, revision, page, info)
}

#[cfg(windows)]
#[tauri::command]
async fn replace_pdf_image_copy(app: tauri::AppHandle, service: State<'_, PdfService>, selections: State<'_, image_edit_commands::ImageReplacementSelections>, selection_id: String, document_id: u64, revision: u64) -> Result<Option<service::SavedCopy>, String> {
    let (target, attempt) = selections.begin(&selection_id, document_id, revision)?;
    let result = async {
        let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
        let source_window = window.clone();
        let source = service.image_replacement_source_via(document_id, revision, target.page, || async move {
            tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&source_window).set_title("Choose an exact-size replacement PNG or JPEG").add_filter("PNG and JPEG images", &["png", "jpg", "jpeg"]).pick_file()).await.map_err(|error| error.to_string())
        }).await?;
        let Some(source) = source else { return Ok(None); };
        let pixel_width = target.pixel_width; let pixel_height = target.pixel_height;
        let replacement = tauri::async_runtime::spawn_blocking(move || {
            let bytes = image_pdf_commands::read_verified_source(&source)?;
            image_pdf::replacement_rgb(bytes, pixel_width, pixel_height)
        }).await.map_err(|error| error.to_string())??;
        let current = service.inspect_image_replacement(document_id, revision, target.page).await?;
        if current.pixel_width != target.pixel_width || current.pixel_height != target.pixel_height || current.page_width != target.display_width || current.page_height != target.display_height { return Err("The image replacement target changed. Inspect it again.".into()); }
        let output = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save PDF with replaced image as a new file").add_filter("PDF documents", &["pdf"]).set_file_name("image-replaced.pdf").save_file()).await.map_err(|error| error.to_string())?;
        let Some(output) = output else { return Ok(None); };
        image_pdf_commands::validate_output_path(&output)?;
        service.replace_image_copy(document_id, revision, target.page, current, replacement, output).await.map(Some)
    }.await;
    if matches!(&result, Ok(Some(_))) { attempt.commit_success()?; }
    result
}

#[cfg(windows)]
#[tauri::command]
fn cancel_image_replacement(selections: State<'_, image_edit_commands::ImageReplacementSelections>, selection_id: String) -> Result<(), String> { selections.cancel(&selection_id) }

#[cfg(windows)]
#[tauri::command]
async fn inspect_text_replacement_target(service:State<'_,PdfService>,selections:State<'_,text_edit_commands::TextReplacementSelections>,document_id:u64,revision:u64,page:u16)->Result<text_edit_commands::TextReplacementTarget,String>{let info=service.inspect_text_replacement(document_id,revision,page).await?;selections.register(document_id,revision,page,info.target.text,info.bounds)}

#[cfg(windows)]
#[tauri::command]
async fn replace_pdf_text_copy(app:tauri::AppHandle,service:State<'_,PdfService>,selections:State<'_,text_edit_commands::TextReplacementSelections>,selection_id:String,document_id:u64,revision:u64,run_id:String,replacement:String)->Result<Option<service::SavedCopy>,String>{let(target,attempt)=selections.begin(&selection_id,document_id,revision)?;if run_id!=target.run_id{return Err("The text replacement run is unavailable.".into())}let current=service.inspect_text_replacement(document_id,revision,target.page).await?;if current.target.text!=target.text||current.bounds!=target.bounds{return Err("The text replacement target changed. Inspect it again.".into())}let window=app.get_webview_window("main").ok_or("Application window is unavailable")?;let path=tauri::async_runtime::spawn_blocking(move||rfd::FileDialog::new().set_parent(&window).set_title("Save PDF with replaced text as a new file").add_filter("PDF documents",&["pdf"]).set_file_name("text-replaced.pdf").save_file()).await.map_err(|e|e.to_string())?;let result=match path{Some(path)=>{image_pdf_commands::validate_output_path(&path)?;service.replace_text_copy(document_id,revision,target.page,current,replacement,path).await.map(Some)},None=>Ok(None)};if matches!(&result,Ok(Some(_))){attempt.commit_success()?}result}

#[cfg(windows)]
#[tauri::command]
fn cancel_text_replacement(selections:State<'_,text_edit_commands::TextReplacementSelections>,selection_id:String)->Result<(),String>{selections.cancel(&selection_id)}

#[tauri::command]
async fn split_document(app: tauri::AppHandle, service: State<'_, PdfService>, id: u64, revision: u64, pages_per_file: usize) -> Result<Option<split::SplitOutput>, String> {
    service.split_via(id, revision, pages_per_file, || async move {
        let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
        let owner = window.hwnd().map_err(|error| error.to_string())?.0 as usize;
        tauri::async_runtime::spawn_blocking(move || split::pick_new_split_folder(owner as *mut std::ffi::c_void, "Choose a folder for split PDFs")).await.map_err(|error| error.to_string())?
    }).await
}

#[tauri::command]
async fn combine_documents(app: tauri::AppHandle, service: State<'_, PdfService>, first: service::CombineSource, second: service::CombineSource) -> Result<Option<service::SavedCopy>, String> {
    service.check_combine(first, second).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save combined PDF as a new file").add_filter("PDF documents", &["pdf"]).set_file_name("combined.pdf").save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.combine(first, second, path).await.map(Some), None => Ok(None) }
}

#[tauri::command]
async fn insert_pages_copy(app: tauri::AppHandle, service: State<'_, PdfService>, target: service::CombineSource, donor: service::CombineSource, at: usize) -> Result<Option<service::SavedCopy>, String> {
    service.check_insertion(target, donor, at).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save PDF with inserted pages as a new file").add_filter("PDF documents", &["pdf"]).set_file_name("inserted-pages.pdf").save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.insert_pages_copy(target, donor, at, path).await.map(Some), None => Ok(None) }
}

#[tauri::command]
async fn replace_pages_copy(app: tauri::AppHandle, service: State<'_, PdfService>, target: service::CombineSource, donor: service::CombineSource, start: usize, count: usize) -> Result<Option<service::SavedCopy>, String> {
    service.check_replacement(target, donor, start, count).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save PDF with replaced pages as a new file").add_filter("PDF documents", &["pdf"]).set_file_name("replaced-pages.pdf").save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.replace_pages_copy(target, donor, start, count, path).await.map(Some), None => Ok(None) }
}

#[tauri::command]
async fn document_form_fields(service: State<'_, PdfService>, id: u64, revision: u64) -> Result<forms::FormFields, String> {
    service.form_fields(id, revision).await
}

#[tauri::command]
async fn fill_form_copy(app: tauri::AppHandle, service: State<'_, PdfService>, id: u64, revision: u64, values: Vec<forms::FieldValue>) -> Result<Option<service::SavedCopy>, String> {
    service.check_form_copy(id, revision, values.clone()).await?;
    let window = app.get_webview_window("main").ok_or("Application window is unavailable")?;
    let path = tauri::async_runtime::spawn_blocking(move || rfd::FileDialog::new().set_parent(&window).set_title("Save filled form as a new PDF").add_filter("PDF documents", &["pdf"]).set_file_name("filled-form.pdf").save_file()).await.map_err(|error| error.to_string())?;
    match path { Some(path) => service.fill_form_copy(id, revision, values, path).await.map(Some), None => Ok(None) }
}

fn main() {
    #[cfg(all(debug_assertions, windows))]
    if pdf_helper::helper_mode_requested(std::env::args_os()) {
        pdf_helper::run_helper_mode();
    }
    #[cfg_attr(not(debug_assertions), allow(unused_mut))]
    let mut context = tauri::generate_context!();
    #[cfg(debug_assertions)]
    let test_dirs = data_dirs::test_override().unwrap_or_else(|error| { eprintln!("{error}"); std::process::exit(2) });
    #[cfg(debug_assertions)]
    if test_dirs.is_some() { data_dirs::defer_configured_windows(&mut context); }
    tauri::Builder::default().plugin(tauri_plugin_updater::Builder::new().build()).manage(update_commands::PendingUpdate::default()).setup(move |app| {
        #[cfg(debug_assertions)]
        if let Some(dirs) = &test_dirs { data_dirs::create_windows(app, dirs)?; }
        let library = app.path().resource_dir()?.join("resources/pdfium/bin/pdfium.dll");
        let recovery_root = data_dirs::app_data_dir(app.handle())?.join("recovery");
        std::fs::create_dir_all(&recovery_root)?;
        let service = PdfService::start_with_recovery(library, recovery_root);
        #[cfg(windows)]
        {
            let commands = ocr_commands::OcrCommands::new(service.clone(), app.path().resource_dir()?);
            let close_commands = commands.clone();
            if let Some(window) = app.get_webview_window("main") {
                window.on_window_event(move |event| {
                    if matches!(event, tauri::WindowEvent::Destroyed) { close_commands.cancel_active(); }
                });
            }
            app.manage(commands);
        }
        app.manage(service);
        app.manage(open_path::OpenPathGrants::default());
        if let Some(window) = app.get_webview_window("main") {
            let handle = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) = event {
                    if let Some(grants) = handle.try_state::<open_path::OpenPathGrants>() { for path in paths { grants.grant(path); } }
                }
            });
        }
        app.manage(print_commands::PrintJobs::default());
        #[cfg(windows)]
        app.manage(image_pdf_commands::ImagePdfSelections::default());
        #[cfg(windows)]
        app.manage(image_edit_commands::ImageReplacementSelections::default());
        app.manage(text_edit_commands::TextReplacementSelections::default());
        app.manage(recovery_commands::RecoveryCommands::default());
        Ok(())
    }).invoke_handler(tauri::generate_handler![recovery_commands::checkpoint_recovery, recovery_commands::restore_recovery, recovery_commands::keep_recovered_edits, recovery_commands::open_original, recovery_commands::discard_recovery, read_update_attempt, write_update_attempt, clear_update_attempt, update_commands::check_for_update, update_commands::download_update, update_commands::install_update, update_commands::release_update, document_form_fields, fill_form_copy, open_document, reopen_document, open_example, render_page, export_page_image, retry_pdf_engine, close_document, edit_pages, crop_page, crop_pages, reset_crops, create_pdf_from_image, choose_image_pdf_sources, create_pdf_from_images, cancel_image_pdf_sources, inspect_image_replacement_target, replace_pdf_image_copy, cancel_image_replacement, inspect_text_replacement_target, replace_pdf_text_copy, cancel_text_replacement, create_comment, update_comment, delete_comment, document_comments, document_annotations, create_highlight, create_text_highlight, update_highlight, delete_highlight, save_copy, redact_document, split_document, combine_documents, insert_pages_copy, replace_pages_copy, page_text, page_text_geometry, document_bookmarks, document_page_labels, document_properties, dependency_notices, unlock_document, cancel_password_request, ocr_capability, recognize_page_ocr, create_searchable_ocr_copy, cancel_page_ocr, print_commands::print_document, print_commands::cancel_print])
      .run(context).expect("Desktop application failed");
}
