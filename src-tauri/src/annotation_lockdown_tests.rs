//! Annotation interchange phase 1, slice 1a (SM3): entry checks before any phase 1 code.
//! - 0a: the bundled PDFium is the non-V8, non-XFA build, and loading, rendering and printing a file full of
//!   actions starts no process while a form-fill handle exists.
//! - 0b: how PDFium draws an app-made text highlight whose `/AP` was removed, in the app's quad order and in
//!   Acrobat's.
//! - The read-only annotation census over a folder of real PDFs (ignored; prints counts only).
use lopdf::{dictionary, Dictionary, Document, Object, Stream, StringFormat};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn root() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")) }
fn library() -> PathBuf { root().join("resources/pdfium/bin/pdfium.dll") }

/// Export names of a PE32+ DLL. Fails rather than returning an empty list on a malformed image.
fn pe_exports(image: &[u8]) -> Result<Vec<String>, String> {
    let u16_at = |at: usize| image.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or("truncated image");
    let u32_at = |at: usize| image.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or("truncated image");
    if image.get(..2) != Some(b"MZ") { return Err("not an MZ image".into()); }
    let pe = u32_at(0x3c)? as usize;
    if image.get(pe..pe + 4) != Some(b"PE\0\0") { return Err("no PE signature".into()); }
    let coff = pe + 4;
    let sections = u16_at(coff + 2)? as usize;
    let optional_size = u16_at(coff + 16)? as usize;
    let optional = coff + 20;
    if u16_at(optional)? != 0x20b { return Err("not PE32+".into()); }
    let export_rva = u32_at(optional + 112)? as usize;
    if export_rva == 0 { return Err("no export directory".into()); }
    let table = optional + optional_size;
    let to_offset = |rva: usize| -> Result<usize, String> {
        for index in 0..sections {
            let header = table + index * 40;
            let (size, address, raw) = (u32_at(header + 8)? as usize, u32_at(header + 12)? as usize, u32_at(header + 20)? as usize);
            if rva >= address && rva < address + size.max(1) { return Ok(raw + (rva - address)); }
        }
        Err(format!("RVA {rva:#x} is in no section"))
    };
    let directory = to_offset(export_rva)?;
    let count = u32_at(directory + 24)? as usize;
    let names = to_offset(u32_at(directory + 32)? as usize)?;
    (0..count).map(|index| {
        let start = to_offset(u32_at(names + index * 4)? as usize)?;
        let end = image[start..].iter().position(|byte| *byte == 0).ok_or("unterminated export name")? + start;
        String::from_utf8(image[start..end].to_vec()).map_err(|_| "export name is not UTF-8".into())
    }).collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool { haystack.windows(needle.len()).any(|window| window == needle) }

/// SM7 A2(a), non-vacuous: each negative check is paired with a positive control that the same scan must find.
#[test]
fn bundled_pdfium_is_the_non_v8_non_xfa_build_and_the_check_is_not_vacuous() {
    let image = std::fs::read(library()).expect("run scripts/setup-pdfium.ps1 first");
    let exports = pe_exports(&image).unwrap();
    assert!(exports.len() > 100, "the export scan found only {} names", exports.len());
    for present in ["FPDF_InitLibrary", "FPDF_InitLibraryWithConfig", "FPDFDOC_InitFormFillEnvironment", "FPDF_FFLDraw", "FORM_DoDocumentOpenAction"] {
        assert!(exports.iter().any(|name| name == present), "positive control: {present} must be exported");
    }
    // FPDF_LoadXFA is a stub exported by every build; the XFA-only exports are the BStr helpers.
    for xfa_only in ["FPDF_BStr_Init", "FPDF_BStr_Set", "FPDF_BStr_Clear"] {
        assert!(!exports.iter().any(|name| name == xfa_only), "{xfa_only} is exported: this is an XFA build");
    }
    assert!(contains(&image, b"FPDFDOC_InitFormFillEnvironment"), "positive control for the byte scan");
    for v8_marker in [b"Maximum call stack size exceeded".as_slice(), b"SharedArrayBuffer", b"WebAssembly"] {
        assert!(!contains(&image, v8_marker), "the DLL holds the V8 string {:?}", String::from_utf8_lossy(v8_marker));
    }
    let args = std::fs::read_to_string(root().join("resources/pdfium/args.gn")).expect("args.gn ships in the verified archive");
    let set = |key: &str| args.lines().find_map(|line| line.split_once('=').filter(|(name, _)| name.trim() == key).map(|(_, value)| value.trim().to_owned()));
    assert_eq!(set("target_cpu").as_deref(), Some("\"x64\""), "positive control: args.gn parses");
    assert_eq!(set("pdf_enable_v8").as_deref(), Some("false"));
    assert_eq!(set("pdf_enable_xfa").as_deref(), Some("false"));
}

/// The `cmd.exe` parameters a Launch action in the hostile file asks for; the control runs exactly these.
fn command_line(marker: &Path) -> String { format!("/c type nul > \"{}\"", marker.display()) }

fn literal(text: &str) -> Object { Object::String(text.as_bytes().to_vec(), StringFormat::Literal) }

/// One page carrying every action route PDFium could take on load, render or print: a document open action,
/// document and page additional actions, document JavaScript, a URI link chained to JavaScript, a Launch link,
/// a Screen with page-visible actions and a text field with keystroke, format and focus actions.
fn hostile_actions_pdf(marker: &Path) -> Vec<u8> {
    let command = command_line(marker);
    let launch = || dictionary! { "S" => "Launch", "F" => literal("cmd.exe"), "Win" => dictionary! { "F" => literal("cmd.exe"), "P" => literal(&command) } };
    let script = |code: &str| dictionary! { "S" => "JavaScript", "JS" => literal(code) };
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let page = document.new_object_id();
    let contents = document.add_object(Stream::new(dictionary! {}, b"BT /F1 12 Tf 20 180 Td (actions) Tj ET\n".to_vec()));
    let font = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let uri = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![10.into(), 10.into(), 60.into(), 30.into()], "P" => page,
        "A" => dictionary! { "S" => "URI", "URI" => literal("https://example.invalid/sm3-lockdown"), "Next" => script("app.launchURL('https://example.invalid/sm3-next');") } });
    let link = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link", "Rect" => vec![10.into(), 40.into(), 60.into(), 60.into()], "P" => page, "A" => launch() });
    let screen = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Screen", "Rect" => vec![70.into(), 10.into(), 120.into(), 60.into()], "P" => page, "F" => 4,
        "A" => launch(), "AA" => dictionary! { "PO" => launch(), "PV" => script("app.launchURL('https://example.invalid/sm3-pv');"), "E" => launch() } });
    let field = document.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Widget", "FT" => "Tx", "T" => literal("f1"), "V" => literal("value"), "F" => 4, "P" => page,
        "Rect" => vec![10.into(), 100.into(), 150.into(), 120.into()], "DA" => literal("/Helv 10 Tf 0 g"),
        "AA" => dictionary! { "K" => script("app.launchURL('https://example.invalid/sm3-k');"), "F" => script("app.launchURL('https://example.invalid/sm3-f');"), "Fo" => launch(), "C" => launch() } });
    document.objects.insert(page, Object::Dictionary(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Contents" => contents, "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        "Annots" => vec![uri.into(), link.into(), screen.into(), field.into()], "AA" => dictionary! { "O" => launch(), "C" => launch() } }));
    document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
    let helv = document.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages, "OpenAction" => launch(),
        "AA" => dictionary! { "WC" => launch(), "WP" => launch(), "DP" => script("app.launchURL('https://example.invalid/sm3-dp');") },
        "Names" => dictionary! { "JavaScript" => dictionary! { "Names" => vec![literal("doc"), Object::Dictionary(script("app.launchURL('https://example.invalid/sm3-doc');"))] } },
        "AcroForm" => dictionary! { "Fields" => vec![field.into()], "DA" => literal("/Helv 0 Tf 0 g"), "DR" => dictionary! { "Font" => dictionary! { "Helv" => helv } } } });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[cfg(windows)]
struct OwnJob(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl OwnJob {
    /// Puts this process into a new job, so every process it (or code inside it) starts afterwards is counted.
    fn enter() -> Self {
        use windows::Win32::System::{JobObjects::{AssignProcessToJobObject, CreateJobObjectW}, Threading::GetCurrentProcess};
        let job = unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }.unwrap();
        unsafe { AssignProcessToJobObject(job, GetCurrentProcess()) }.unwrap();
        Self(job)
    }
    fn total_processes(&self) -> u32 {
        use windows::Win32::System::JobObjects::{JobObjectBasicAccountingInformation, QueryInformationJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION};
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        unsafe { QueryInformationJobObject(Some(self.0), JobObjectBasicAccountingInformation, &mut info as *mut _ as *mut _, std::mem::size_of_val(&info) as u32, None) }.unwrap();
        info.TotalProcesses
    }
}

/// SM7 A2(b)(c). pdfium-render 0.9.4 binds `FPDFDOC_InitFormFillEnvironment` on every load with every
/// `FPDF_FORMFILLINFO` callback `None` and a null `m_pJsPlatform`, and never calls a `FORM_*` action or event
/// function; the app calls none either. This checks the effect: with a form handle bound, loading, viewer,
/// print and plain renders, and text reads of the hostile file start no process (job accounting) and the
/// Launch target never writes its marker. A control then starts one process the same way a Launch would, to
/// prove both detectors work.
#[cfg(windows)]
#[test]
fn pdfium_with_a_form_handle_runs_no_action_and_starts_no_process() {
    use crate::pdf_engine::{EngineDocument, PageTarget, PdfEngine, RenderMode};
    const CHILD: &str = "SM3_ANNOT_LOCKDOWN_CHILD";
    const NAME: &str = "comments::lockdown_tests::pdfium_with_a_form_handle_runs_no_action_and_starts_no_process";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::child_job::test_output(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"]).env(CHILD, "1")).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("annotation-lockdown-ok"), "child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    let job = OwnJob::enter();
    assert_eq!(job.total_processes(), 1, "the job holds only this test process");
    struct RemoveOnDrop(PathBuf);
    impl Drop for RemoveOnDrop { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    let folder = RemoveOnDrop(std::env::temp_dir().join(format!("sm3-annot-lockdown-{}", std::process::id())));
    std::fs::create_dir_all(&folder.0).unwrap();
    let marker = folder.0.join("launched.txt");
    let bytes = hostile_actions_pdf(&marker);
    let engine = crate::pdf_engine::InProcessEngine::bind(library()).unwrap();
    assert!(!engine.load_from_vec(blank_page(), None).unwrap().has_form_handle(), "control: a file without /AcroForm binds no form handle");
    for _ in 0..2 {
        let document = engine.load_from_vec(bytes.clone(), None).unwrap();
        assert!(document.has_form_handle(), "the check must run with a form-fill environment bound");
        for mode in [RenderMode::Viewer, RenderMode::Print, RenderMode::Plain] {
            let mut out = vec![0u8; 200 * 200 * 4];
            document.render_bgra_into(PageTarget::Source(0), mode, 200, 200, &mut out).unwrap();
        }
        let spec = crate::editor::PageSpec { source: 0, turns: 0, crop: None, notes: Vec::new() };
        document.render_viewer_png(&spec, 400).unwrap();
        document.visible_text(PageTarget::Source(0)).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(job.total_processes(), 1, "PDFium started a process");
    assert!(!marker.exists(), "a Launch action ran");
    use std::os::windows::process::CommandExt;
    let status = crate::child_job::test_status(std::process::Command::new("cmd.exe").raw_arg(&command_line(&marker))).unwrap();
    assert!(status.success());
    assert_eq!(job.total_processes(), 2, "control: a started process is counted");
    assert!(marker.exists(), "control: the Launch command line writes its marker");
    drop(folder);
    println!("annotation-lockdown-ok");
}

fn blank_page() -> Vec<u8> {
    let mut document = Document::with_version("1.7");
    let pages = document.new_object_id();
    let contents = document.add_object(Stream::new(dictionary! {}, Vec::new()));
    let page = document.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()], "Contents" => contents, "Resources" => dictionary! {} });
    document.objects.insert(pages, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }));
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

fn highlight_annotation(document: &mut Document) -> &mut Dictionary {
    let page = document.get_pages()[&1];
    let id = document.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap();
    document.get_dictionary_mut(id).unwrap()
}

/// Design task 0b (1.3). Through 0.3.x the app wrote QuadPoints BL, BR, TR, TL; Acrobat and PDFium's AP
/// generator use TL, TR, BL, BR, which the app writes since slice 1b. This measures what a reader that
/// regenerates the appearance draws once `/AP` is gone. Coverage = share of pixels inside each quad (inset
/// 2 px) that are highlighted (blue channel below 230 on a white page).
#[test]
fn app_text_highlight_without_its_appearance_in_pdfium() {
    use crate::pdf_engine::{EngineDocument, PageTarget, PdfEngine, RenderMode};
    const CHILD: &str = "SM3_ANNOT_NO_AP_CHILD";
    const NAME: &str = "comments::lockdown_tests::app_text_highlight_without_its_appearance_in_pdfium";
    if std::env::var_os(CHILD).is_none() {
        let output = crate::child_job::test_output(std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture", "--test-threads=1"]).env(CHILD, "1")).unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success() && stdout.contains("no-ap-probe-ok"), "child failed:\n{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
        if let Some(line) = stdout.lines().find(|line| line.contains("no-ap-probe-ok")) { println!("{line}"); }
        return;
    }
    let quads = [crate::editor::CropBox { left: 40.0, bottom: 140.0, right: 160.0, top: 160.0 }, crate::editor::CropBox { left: 40.0, bottom: 100.0, right: 120.0, top: 120.0 }];
    let mut session = crate::editor::EditSession::new(blank_page(), 1);
    let next = session.proposed_text_highlight(0, quads.to_vec(), None).unwrap();
    session.commit_comments(next);
    let written = Document::load_mem(&session.export(None).unwrap()).unwrap();
    let points = |document: &Document| { let mut document = document.clone(); highlight_annotation(&mut document).get(b"QuadPoints").unwrap().as_array().unwrap().iter().map(|value| value.as_float().unwrap()).collect::<Vec<f32>>() };
    assert_eq!(points(&written), vec![40.0, 160.0, 160.0, 160.0, 40.0, 140.0, 160.0, 140.0, 40.0, 120.0, 120.0, 120.0, 40.0, 100.0, 120.0, 100.0], "the app writes TL, TR, BL, BR");
    let save = |document: &Document| { let mut bytes = Vec::new(); document.clone().save_to(&mut bytes).unwrap(); bytes };
    let mut acrobat_order = written.clone();
    highlight_annotation(&mut acrobat_order).remove(b"AP");
    let mut app_order = acrobat_order.clone();
    let legacy: Vec<Object> = quads.iter().flat_map(|q| [q.left, q.bottom, q.right, q.bottom, q.right, q.top, q.left, q.top]).map(Object::Real).collect();
    highlight_annotation(&mut app_order).set("QuadPoints", legacy);
    let engine = crate::pdf_engine::InProcessEngine::bind(library()).unwrap();
    let coverage = |bytes: Vec<u8>| -> Vec<f64> {
        let document = engine.load_from_vec(bytes, None).unwrap();
        let mut out = vec![0u8; 400 * 400 * 4];
        document.render_bgra_into(PageTarget::Source(0), RenderMode::Viewer, 400, 400, &mut out).unwrap();
        quads.iter().map(|quad| {
            let (x0, x1, y0, y1) = ((quad.left * 2.0) as usize + 2, (quad.right * 2.0) as usize - 2, ((200.0 - quad.top) * 2.0) as usize + 2, ((200.0 - quad.bottom) * 2.0) as usize - 2);
            let (mut lit, mut all) = (0usize, 0usize);
            for y in y0..y1 { for x in x0..x1 { all += 1; if out[(y * 400 + x) * 4] < 230 { lit += 1; } } }
            lit as f64 / all as f64
        }).collect()
    };
    let with_ap = coverage(save(&written));
    let app = coverage(save(&app_order));
    let acrobat = coverage(save(&acrobat_order));
    println!("no-ap-probe-ok: coverage per quad, with /AP {with_ap:.3?}; no /AP, as written now (Acrobat order) {acrobat:.3?}; no /AP, 0.3.x order {app:.3?}");
    assert!(with_ap.iter().all(|value| *value > 0.99), "the app's own appearance fills each quad");
    assert!(acrobat.iter().all(|value| *value > 0.99), "PDFium regenerates the app's highlights fully once /AP is gone");
    assert!(app.iter().all(|value| *value < 0.9), "PDFium misdraws the 0.3.x quad order without /AP (pinned; if this changes, update design 1.3)");
    let mut legacy_file = app_order.clone();
    let id = { let page = legacy_file.get_pages()[&1]; legacy_file.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap()[0].as_reference().unwrap() };
    legacy_file.get_dictionary_mut(id).unwrap().set("AP", written.get_dictionary(id).unwrap().get(b"AP").unwrap().clone());
    assert_eq!(crate::comments::read(&legacy_file).unwrap()[0], session.plan[0].notes, "a 0.3.x file (old quad order, own /AP) still reads");
}

const CENSUS_KNOWN: &[&str] = &["Text", "Link", "FreeText", "Line", "Square", "Circle", "Polygon", "PolyLine", "Highlight", "Underline", "Squiggly", "StrikeOut", "Stamp", "Caret", "Ink", "Popup", "FileAttachment", "Sound", "Movie", "Widget", "Screen", "PrinterMark", "TrapNet", "Watermark", "3D", "Redact", "RichMedia", "Projection"];

fn producer_family(document: &Document) -> &'static str {
    let producer = document.trailer.get(b"Info").ok().and_then(|info| document.dereference(info).ok()).and_then(|(_, info)| info.as_dict().ok())
        .and_then(|info| info.get(b"Producer").ok()).and_then(|value| value.as_str().ok()).map(|bytes| {
            if bytes.starts_with(&[0xfe, 0xff]) { String::from_utf16_lossy(&bytes[2..].chunks_exact(2).map(|pair| u16::from_be_bytes([pair[0], pair[1]])).collect::<Vec<_>>()) } else { String::from_utf8_lossy(bytes).into_owned() }
        }).map(|text| text.to_lowercase());
    let Some(producer) = producer else { return "none" };
    for (needle, family) in [("acrobat", "adobe"), ("adobe pdf library", "adobe"), ("distiller", "adobe"), ("adobe", "adobe"), ("microsoft", "microsoft"), ("itext", "itext"), ("pdfium", "pdfium"), ("ghostscript", "ghostscript"), ("quartz", "apple"), ("foxit", "foxit"), ("nitro", "nitro"), ("bluebeam", "bluebeam"), ("kofax", "kofax"), ("tesseract", "tesseract"), ("libreoffice", "libreoffice"), ("openoffice", "libreoffice"), ("pdf-xchange", "pdf-xchange"), ("tracker", "pdf-xchange"), ("reportlab", "reportlab"), ("pdf workstation", "this app"), ("canon", "scanner"), ("xerox", "scanner"), ("ricoh", "scanner"), ("konica", "scanner"), ("kyocera", "scanner"), ("sharp", "scanner"), ("hp ", "scanner"), ("epson", "scanner"), ("brother", "scanner"), ("lopdf", "lopdf"), ("qpdf", "qpdf"), ("pdfsharp", "pdfsharp"), ("aspose", "aspose")] {
        if producer.contains(needle) { return family; }
    }
    "other"
}

fn quad_order(points: &[f32]) -> &'static str {
    let [x1, y1, x2, y2, x3, y3, x4, y4] = [points[0], points[1], points[2], points[3], points[4], points[5], points[6], points[7]];
    let near = |a: f32, b: f32| (a - b).abs() <= 0.01 * (1.0 + a.abs().max(b.abs()));
    if near(y1, y2) && near(y3, y4) && near(x1, x3) && near(x2, x4) && y1 >= y3 { return "acrobat (TL,TR,BL,BR)"; }
    if near(y1, y2) && near(y3, y4) && near(x2, x3) && near(x1, x4) && y3 >= y1 { return "counter-clockwise (BL,BR,TR,TL)"; }
    if near(y1, y2) && near(y3, y4) { return "axis-aligned, other order"; }
    "rotated or skewed"
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool { use std::os::windows::fs::MetadataExt; metadata.file_attributes() & 0x400 != 0 || metadata.file_type().is_symlink() }
#[cfg(not(windows))]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool { metadata.file_type().is_symlink() }

#[test]
fn census_walk_skips_reparse_points() {
    let folder = std::env::temp_dir().join(format!("sm3-census-walk-{}", std::process::id()));
    std::fs::create_dir_all(folder.join("real")).unwrap();
    let plain = std::fs::metadata(folder.join("real")).unwrap();
    assert!(!is_reparse_point(&plain), "an ordinary folder is walked");
    #[cfg(windows)]
    {
        let junction = folder.join("junction");
        let made = crate::child_job::test_output(std::process::Command::new("cmd.exe").args(["/c", "mklink", "/J"]).arg(&junction).arg(folder.join("real"))).unwrap();
        assert!(made.status.success(), "mklink /J failed");
        let entry = std::fs::read_dir(&folder).unwrap().flatten().find(|entry| entry.file_name() == "junction").unwrap();
        assert!(is_reparse_point(&entry.metadata().unwrap()), "a junction is skipped");
    }
    std::fs::remove_dir_all(&folder).unwrap();
}

/// Read-only census of annotations in a folder of real PDFs (design E7, E12). Prints counts and fixed
/// buckets only: never a file name, path, author, body, URI, attachment name or raw producer string.
/// `SM3_ANNOT_CENSUS_DIR=<folder> cargo test --release --bins annotation_census -- --ignored --nocapture`
#[test]
#[ignore = "manual read-only census: set SM3_ANNOT_CENSUS_DIR"]
fn annotation_census_real_files() {
    let Ok(folder) = std::env::var("SM3_ANNOT_CENSUS_DIR") else { return };
    let started = std::time::Instant::now();
    let mut pending = vec![PathBuf::from(folder)];
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |key: String, amount: usize| *counts.entry(key).or_insert(0) += amount;
    let (mut files, mut unreadable, mut loaded, mut annotated, mut max_per_file) = (0usize, 0usize, 0usize, 0usize, 0usize);
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            // DirEntry metadata does not follow links; reparse points (junctions, symlinks, cloud
            // placeholders) are skipped so the walk never leaves the folder it was given.
            let Ok(metadata) = entry.metadata() else { unreadable += 1; continue };
            if is_reparse_point(&metadata) { add("skipped: reparse point".into(), 1); continue; }
            if metadata.is_dir() { pending.push(path); continue; }
            if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("pdf")) { continue; }
            files += 1;
            if metadata.len() > 512 * 1024 * 1024 { add("skipped: over 512 MiB".into(), 1); continue; }
            let Ok(bytes) = std::fs::read(&path) else { unreadable += 1; continue };
            let Ok(document) = crate::pdf_load::load(&bytes) else { add("files: refused by pdf_load".into(), 1); continue };
            loaded += 1;
            let family = producer_family(&document);
            let mut in_file = 0usize;
            for (_, page) in document.get_pages() {
                let Ok(page) = document.get_dictionary(page) else { continue };
                let Ok(annots) = page.get(b"Annots") else { continue };
                let annots = match annots { Object::Reference(id) => { add("pages: indirect /Annots array".into(), 1); document.get_object(*id).ok().and_then(|value| value.as_array().ok()) }, other => other.as_array().ok() };
                let Some(annots) = annots else { add("pages: /Annots not an array".into(), 1); continue };
                for entry in annots {
                    let annotation = match entry { Object::Reference(id) => document.get_dictionary(*id).ok(), Object::Dictionary(dict) => { add("annotations: inline dictionary".into(), 1); Some(dict) }, _ => None };
                    let Some(annotation) = annotation else { add("annotations: null or non-dictionary entry".into(), 1); continue };
                    in_file += 1;
                    let subtype = annotation.get(b"Subtype").and_then(Object::as_name).ok().and_then(|name| std::str::from_utf8(name).ok()).filter(|name| CENSUS_KNOWN.contains(name)).unwrap_or("(other or missing)");
                    add(format!("subtype: {subtype}"), 1);
                    if subtype == "Widget" { continue; }
                    add(format!("producer family of non-widget annotations: {family}"), 1);
                    for (key, label) in [(b"AP".as_slice(), "/AP"), (b"T", "/T author"), (b"RC", "/RC rich text"), (b"IRT", "/IRT reply or state"), (b"Popup", "/Popup"), (b"NM", "/NM"), (b"CreationDate", "/CreationDate"), (b"M", "/M"), (b"Subj", "/Subj"), (b"CA", "/CA"), (b"BS", "/BS"), (b"BE", "/BE"), (b"DS", "/DS"), (b"IT", "/IT"), (b"State", "/State"), (b"AA", "/AA"), (b"A", "/A"), (b"OC", "/OC"), (b"StructParent", "/StructParent"), (b"Rotate", "/Rotate (non-standard)")] {
                        if annotation.has(key) { add(format!("key present: {label}"), 1); }
                    }
                    if !annotation.has(b"AP") && subtype != "Popup" && subtype != "Link" { add(format!("no /AP: {subtype}"), 1); }
                    if let Ok(flags) = annotation.get(b"F").and_then(Object::as_i64) { if flags & 2 != 0 { add("flag: Hidden".into(), 1); } if flags & 32 != 0 { add("flag: NoView".into(), 1); } if flags & 128 != 0 { add("flag: Locked".into(), 1); } }
                    if matches!(subtype, "Highlight" | "Underline" | "StrikeOut" | "Squiggly" | "Redact" | "Link") {
                        if let Ok(points) = annotation.get(b"QuadPoints").and_then(Object::as_array) {
                            let values = points.iter().filter_map(|value| value.as_float().ok()).collect::<Vec<_>>();
                            if values.len() == points.len() && !values.is_empty() && values.len() % 8 == 0 { for quad in values.chunks_exact(8) { add(format!("quad order ({}): {}", if subtype == "Link" { "Link" } else { "markup" }, quad_order(quad)), 1); } }
                            else { add("quad points: malformed".into(), 1); }
                        }
                    }
                    if let Ok(contents) = annotation.get(b"Contents").and_then(Object::as_str) { add(format!("contents encoding: {}", if contents.starts_with(&[0xfe, 0xff]) { "UTF-16BE" } else { "PDFDocEncoding or other" }), 1); }
                }
            }
            if in_file > 0 {
                annotated += 1;
                max_per_file = max_per_file.max(in_file);
                add(format!("annotated files by producer family: {family}"), 1);
                let survey = crate::annotations::survey(&document);
                add("view-only survey: annotations listed".into(), survey.pages.iter().map(Vec::len).sum());
                add("view-only survey: not listed (over a limit)".into(), survey.truncated);
                add(format!("view-only survey: page count matches: {}", survey.pages.len() == document.get_pages().len()), 1);
                let verdict = match crate::comments::read(&document) { Ok(_) => "accepted", Err(reason) if reason.contains("foreign") => "refused: foreign annotation", Err(reason) if reason.contains("unknown annotation") => "refused: unknown key", Err(reason) if reason.contains("forms, tagged") => "refused: form, tagged, labels or threads", Err(reason) if reason.contains("Signed") => "refused: signed", Err(reason) if reason.contains("Encrypted") => "refused: encrypted", Err(_) => "refused: other" };
                add(format!("today's comments::read on annotated files: {verdict}"), 1);
                let mut bands = [(1usize, "1"), (10, "2-10"), (100, "11-100"), (1_000, "101-1,000"), (usize::MAX, "over 1,000")].into_iter();
                let band = bands.find(|(limit, _)| in_file <= *limit).map(|(_, label)| label).unwrap();
                add(format!("annotations per annotated file: {band}"), 1);
            }
        }
    }
    println!("annotation census: {files} PDF files, {unreadable} unreadable, {loaded} loaded by pdf_load, {annotated} with annotations, at most {max_per_file} in one file, {:?}", started.elapsed());
    for (key, value) in counts { println!("  {key}: {value}"); }
}
