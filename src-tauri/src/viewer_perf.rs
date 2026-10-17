//! Headless viewer-performance harness (SM1). Every test is `#[ignore]`: run on demand in release mode,
//! single-threaded, e.g.
//! `cargo test --release --bin pdf-workstation viewer_perf -- --ignored --nocapture --test-threads=1`.
//!
//! It drives the real `PdfService` worker (the same `open`, `render`, `text` and `bookmarks` calls the Tauri
//! commands make) and a bare PDFium instance for the render/PNG split. Environment:
//! `SM1_PERF_DIR`  scratch folder for the generated synthetic PDFs (required),
//! `SM1_PERF_OUT`  file the result lines are appended to (default: stdout only),
//! `SM1_PERF_REAL` text file with one PDF path per line to time as "real-NN" (paths are never printed),
//! `SM1_PERF_PAGES` comma list of synthetic page counts (default 10,1000,10000,65536).
//! Nothing here reads or writes outside those locations.

use std::{io::Write as _, path::{Path, PathBuf}, time::{Duration, Instant}};
use lopdf::{dictionary, Document, Object, Stream};
use pdfium_render::prelude::*;
use crate::service::PdfService;

#[repr(C)]
#[derive(Default)]
struct ProcessMemoryCounters { cb: u32, page_faults: u32, peak_ws: usize, ws: usize, qpp: usize, qp: usize, qpnp: usize, qnp: usize, pagefile: usize, peak_pagefile: usize, private: usize }
#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> isize;
    fn K32GetProcessMemoryInfo(process: isize, counters: *mut ProcessMemoryCounters, cb: u32) -> i32;
}
/// (working set, private bytes, peak working set) in MiB.
fn memory() -> (f64, f64, f64) {
    let mut c = ProcessMemoryCounters::default(); c.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
    unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb); }
    let m = |b: usize| b as f64 / 1_048_576.0;
    (m(c.ws), m(c.private), m(c.peak_ws))
}

fn out(line: &str) {
    println!("{line}");
    if let Ok(path) = std::env::var("SM1_PERF_OUT") {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) { let _ = writeln!(file, "{line}"); }
    }
}
fn ms(d: Duration) -> f64 { d.as_secs_f64() * 1000.0 }
fn time<T>(f: impl FnOnce() -> T) -> (T, f64) { let t = Instant::now(); let v = f(); (v, ms(t.elapsed())) }
fn stats(mut v: Vec<f64>) -> (f64, f64, f64, f64) {
    if v.is_empty() { return (0.0, 0.0, 0.0, 0.0); }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    (mean, v[v.len() / 2], v[((v.len() as f64 * 0.95) as usize).min(v.len() - 1)], v[v.len() - 1])
}
fn library() -> PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/pdfium/bin/pdfium.dll") }
fn block_on<F: std::future::Future>(f: F) -> F::Output { tauri::async_runtime::block_on(f) }

/// A synthetic letter-size document: `pages` pages with about 1 KB of text each (Flate-compressed), a shared
/// Helvetica, a balanced page tree (`fanout` pages per node, or one flat Kids array when 0) and `outline` bookmarks.
fn generate(path: &Path, pages: usize, fanout: usize, outline: usize) {
    let mut doc = Document::with_version("1.5");
    let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
    let resources = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font } });
    let pages_id = doc.new_object_id();
    let mut leaves = Vec::with_capacity(pages);
    for n in 0..pages {
        let mut content = format!("0.9 g 40 40 532 712 re f 0 g\nBT /F1 14 Tf 72 740 Td 16 TL (Synthetic page {} of {}) Tj\n", n + 1, pages);
        for line in 0..44 { content.push_str(&format!("T* (Line {line} of page {n}: the quick brown fox jumps over the lazy dog 0123456789) Tj\n")); }
        content.push_str("ET\n");
        let mut stream = Stream::new(dictionary! {}, content.into_bytes());
        let _ = stream.compress();
        let content_id = doc.add_object(stream);
        let page = doc.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()], "Resources" => resources, "Contents" => content_id });
        leaves.push(page);
    }
    let kids: Vec<Object> = if fanout == 0 {
        for id in &leaves { if let Ok(Object::Dictionary(d)) = doc.get_object_mut(*id) { d.set("Parent", pages_id); } }
        leaves.iter().map(|id| Object::Reference(*id)).collect()
    } else {
        let mut nodes = Vec::new();
        for group in leaves.chunks(fanout) {
            let node = doc.new_object_id();
            for id in group { if let Ok(Object::Dictionary(d)) = doc.get_object_mut(*id) { d.set("Parent", node); } }
            doc.objects.insert(node, Object::Dictionary(dictionary! { "Type" => "Pages", "Parent" => pages_id, "Kids" => group.iter().map(|id| Object::Reference(*id)).collect::<Vec<_>>(), "Count" => group.len() as i64 }));
            nodes.push(Object::Reference(node));
        }
        nodes
    };
    doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages as i64 }));
    let mut catalog = dictionary! { "Type" => "Catalog", "Pages" => pages_id };
    if outline > 0 {
        let outlines = doc.new_object_id();
        let ids: Vec<_> = (0..outline).map(|_| doc.new_object_id()).collect();
        for (i, id) in ids.iter().enumerate() {
            let target = leaves[i * pages / outline];
            let mut d = dictionary! { "Title" => Object::string_literal(format!("Section {}", i + 1)), "Parent" => outlines, "Dest" => vec![Object::Reference(target), "Fit".into()] };
            if i > 0 { d.set("Prev", ids[i - 1]); }
            if i + 1 < ids.len() { d.set("Next", ids[i + 1]); }
            doc.objects.insert(*id, Object::Dictionary(d));
        }
        doc.objects.insert(outlines, Object::Dictionary(dictionary! { "Type" => "Outlines", "First" => ids[0], "Last" => ids[ids.len() - 1], "Count" => outline as i64 }));
        catalog.set("Outlines", outlines);
    }
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", catalog_id);
    doc.save(path).unwrap();
}

fn measure(label: &str, path: &Path, full_text_scan: bool) {
    let size_mb = std::fs::metadata(path).map(|m| m.len() as f64 / 1_048_576.0).unwrap_or(0.0);
    out(&format!("## {label}  size_mib={size_mb:.2}"));
    let (ws0, pv0, _) = memory();
    open_and_walk(label, path, full_text_scan, ws0, pv0);
}

/// Open stages on the bare engine (PDFium binds once per process, so this runs in its own test).
fn stages(engine: &crate::pdf_engine::InProcessEngine, label: &str, path: &Path) {
    use crate::pdf_engine::{EngineDocument, PdfEngine};
    {
        let (bytes, t_read) = time(|| crate::open_limits::read_open_source(path, std::iter::empty::<u64>(), 32));
        let Ok(bytes) = bytes else { out(&format!("{label} stage_open_error={}", bytes.err().unwrap())); return; };
        let (parsed, t_lopdf) = time(|| crate::pdf_load::load_for_open(&bytes));
        let lopdf_ok = matches!(&parsed, Ok(Ok(_)));
        drop(parsed);
        let copy = bytes.clone();
        let (doc, t_load) = time(|| engine.load_from_vec(copy, None));
        match doc {
            Ok(doc) => {
                let (sizes, t_sizes) = time(|| doc.page_sizes());
                out(&format!("{label} stage_read_ms={t_read:.1} stage_lopdf_ms={t_lopdf:.1} lopdf_ok={lopdf_ok} stage_pdfium_load_ms={t_load:.1} stage_page_sizes_ms={t_sizes:.1} pages={}", sizes.as_ref().map(|s| s.len()).unwrap_or(0)));
            }
            Err(e) => { out(&format!("{label} stage_pdfium_error={e}")); return; }
        }
    }
}

fn open_and_walk(label: &str, path: &Path, full_text_scan: bool, ws0: f64, pv0: f64) {
    let service = PdfService::start(library());
    let (opened, t_open) = time(|| block_on(service.open(path.to_path_buf())));
    let info = match opened { Ok(info) => info, Err(e) => { out(&format!("{label} open_error={e}")); return; } };
    let (json, t_json) = time(|| serde_json::to_vec(&info).unwrap());
    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    let id = value["id"].as_u64().unwrap();
    let revision = value["revision"].as_u64().unwrap();
    let pages = value["pages"].as_array().unwrap().len();
    let (ws1, pv1, _) = memory();
    out(&format!("{label} open_ms={t_open:.1} pages={pages} documentinfo_json_kib={:.1} serialize_ms={t_json:.2} mem_before_ws={ws0:.0} mem_before_private={pv0:.0} mem_open_ws={ws1:.0} mem_open_private={pv1:.0}", json.len() as f64 / 1024.0));

    let render = |page: usize, width: i32| -> (usize, f64) { let (r, t) = time(|| block_on(service.render(id, page as u16, width))); (r.map(|b| b.len()).unwrap_or(0), t) };
    for width in [816, 1224, 1632] {
        let (cold_len, cold) = render(0, width);
        let (_, warm) = render(0, width);
        out(&format!("{label} first_render width={width} cold_ms={cold:.1} warm_cache_ms={warm:.2} png_kib={:.1}", cold_len as f64 / 1024.0));
    }
    let (ws2, pv2, _) = memory();
    // Page-to-page at 1224 px (1.5x devicePixelRatio): cold sequential pages 1.., then warm repeat.
    let span = pages.saturating_sub(1).min(30);
    let mut cold = Vec::new(); let mut sizes = Vec::new();
    for page in 1..=span { let (len, t) = render(page, 1224); cold.push(t); sizes.push(len as f64 / 1024.0); }
    let mut warm = Vec::new();
    for page in 1..=span { warm.push(render(page, 1224).1); }
    let (cm, cmed, cp95, cmax) = stats(cold); let (wm, wmed, ..) = stats(warm); let (sm, smed, _, smax) = stats(sizes);
    out(&format!("{label} page_to_page width=1224 n={span} cold_mean_ms={cm:.1} cold_p50_ms={cmed:.1} cold_p95_ms={cp95:.1} cold_max_ms={cmax:.1} warm_mean_ms={wm:.2} warm_p50_ms={wmed:.2} png_mean_kib={sm:.1} png_p50_kib={smed:.1} png_max_kib={smax:.1}"));
    // Deep jumps: cold render of pages far into the document.
    if pages > 100 {
        for (name, page) in [("25pct", pages / 4), ("50pct", pages / 2), ("90pct", pages * 9 / 10), ("last", pages - 1)] {
            let (len, t) = render(page, 1224);
            out(&format!("{label} jump_render {name} page={} cold_ms={t:.1} png_kib={:.1}", page + 1, len as f64 / 1024.0));
        }
    }
    let (ws3, pv3, _) = memory();
    // Text: sample evenly (or the whole document for the synthetic scans).
    let sample: Vec<usize> = if full_text_scan { (0..pages).collect() } else { let n = pages.min(200); (0..n).map(|i| i * pages / n).collect() };
    let started = Instant::now();
    let mut per = Vec::with_capacity(sample.len()); let mut chars = 0usize; let mut errors = 0usize;
    for &page in &sample {
        let (r, t) = time(|| block_on(service.text(id, page as u16, revision)));
        match r { Ok(t) => chars += t.len(), Err(_) => errors += 1 }
        per.push(t);
        if started.elapsed() > Duration::from_secs(300) { break; }
    }
    let scanned = per.len(); let total = ms(started.elapsed());
    let (tm, tmed, tp95, tmax) = stats(per);
    out(&format!("{label} text scanned={scanned}/{pages} errors={errors} total_ms={total:.0} mean_ms={tm:.3} p50_ms={tmed:.3} p95_ms={tp95:.3} max_ms={tmax:.1} chars_per_page={:.0} est_full_scan_s={:.1}", chars as f64 / scanned.max(1) as f64, tm * pages as f64 / 1000.0));
    // Text geometry (what TextLayer requests for each mounted page): 20 evenly spread pages, cold.
    let geometry_sample: Vec<usize> = { let n = pages.min(20); (0..n).map(|i| i * pages / n).collect() };
    let mut geometry_ms = Vec::new(); let mut geometry_kib = Vec::new();
    for &page in &geometry_sample {
        let (r, t) = time(|| block_on(service.text_geometry(id, page as u16, revision)));
        geometry_ms.push(t);
        if let Ok(g) = r { geometry_kib.push(serde_json::to_vec(&g).map(|v| v.len()).unwrap_or(0) as f64 / 1024.0); }
    }
    let (gm, gmed, gp95, gmax) = stats(geometry_ms); let (km, kmed, _, kmax) = stats(geometry_kib);
    out(&format!("{label} text_geometry n={} mean_ms={gm:.2} p50_ms={gmed:.2} p95_ms={gp95:.2} max_ms={gmax:.1} json_mean_kib={km:.1} json_p50_kib={kmed:.1} json_max_kib={kmax:.1}", geometry_sample.len()));
    let (ws4, pv4, _) = memory();
    let (marks, t_marks) = time(|| block_on(service.bookmarks(id, revision)));
    let count = marks.as_ref().map(|m| serde_json::to_value(m).unwrap()["items"].as_array().map(|a| a.len()).unwrap_or(0)).unwrap_or(0);
    let json_kib = marks.as_ref().map(|m| serde_json::to_vec(m).unwrap().len() as f64 / 1024.0).unwrap_or(0.0);
    out(&format!("{label} bookmarks ms={t_marks:.1} items={count} json_kib={json_kib:.1} err={}", marks.err().unwrap_or_default()));
    let (ws5, pv5, peak) = memory();
    let _ = block_on(service.close(id));
    std::thread::sleep(Duration::from_millis(300));
    let (ws6, pv6, _) = memory();
    out(&format!("{label} memory_mib(ws/private) open={ws1:.0}/{pv1:.0} after_first_renders={ws2:.0}/{pv2:.0} after_30_pages_and_jumps={ws3:.0}/{pv3:.0} after_text={ws4:.0}/{pv4:.0} after_bookmarks={ws5:.0}/{pv5:.0} peak_ws={peak:.0} after_close={ws6:.0}/{pv6:.0}"));
}

#[test]
#[ignore]
fn viewer_perf_service() {
    let dir = PathBuf::from(std::env::var("SM1_PERF_DIR").expect("SM1_PERF_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    out(&format!("# viewer_perf_service logical_cpus={}", std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0)));
    let counts: Vec<usize> = std::env::var("SM1_PERF_PAGES").unwrap_or("10,1000,10000,65536".into()).split(',').filter_map(|s| s.trim().parse().ok()).collect();
    for pages in counts {
        for (kind, fanout) in [("balanced", 100usize), ("flat", 0usize)] {
            if kind == "flat" && pages < 1000 { continue; }
            let path = dir.join(format!("synthetic-{pages}-{kind}.pdf"));
            let (_, t_gen) = time(|| generate(&path, pages, fanout, 1000.min(pages)));
            out(&format!("# generated synthetic {pages} pages {kind} in {t_gen:.0} ms"));
            measure(&format!("synthetic-{pages}-{kind}"), &path, pages >= 10000 && kind == "balanced");
            if std::env::var("SM1_PERF_KEEP").is_err() { let _ = std::fs::remove_file(&path); }
        }
    }
    if let Ok(list) = std::env::var("SM1_PERF_REAL") {
        let paths: Vec<PathBuf> = std::fs::read_to_string(list).unwrap().lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect();
        for (index, path) in paths.iter().enumerate() { measure(&format!("real-{:02}", index + 1), path, false); }
    }
}

#[test]
#[ignore]
fn viewer_perf_stages() {
    let dir = PathBuf::from(std::env::var("SM1_PERF_DIR").expect("SM1_PERF_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let engine = crate::pdf_engine::InProcessEngine::bind(library()).unwrap();
    out("# viewer_perf_stages (bare engine, one open's stages)");
    let counts: Vec<usize> = std::env::var("SM1_PERF_PAGES").unwrap_or("10,1000,10000,65536".into()).split(',').filter_map(|s| s.trim().parse().ok()).collect();
    for pages in counts {
        for (kind, fanout) in [("balanced", 100usize), ("flat", 0usize)] {
            if kind == "flat" && pages < 1000 { continue; }
            let path = dir.join(format!("synthetic-{pages}-{kind}.pdf"));
            generate(&path, pages, fanout, 1000.min(pages));
            stages(&engine, &format!("synthetic-{pages}-{kind}"), &path);
            let _ = std::fs::remove_file(&path);
        }
    }
    if let Ok(list) = std::env::var("SM1_PERF_REAL") {
        for (index, line) in std::fs::read_to_string(list).unwrap().lines().enumerate() { if !line.trim().is_empty() { stages(&engine, &format!("real-{:02}", index + 1), Path::new(line.trim())); } }
    }
}

/// Render / bitmap-to-image / PNG-encode split for the first page of the given documents, on a bare PDFium.
#[test]
#[ignore]
fn viewer_perf_split() {
    let dir = PathBuf::from(std::env::var("SM1_PERF_DIR").expect("SM1_PERF_DIR"));
    std::fs::create_dir_all(&dir).unwrap();
    let pdfium = Pdfium::new(Pdfium::bind_to_library(library()).unwrap());
    let mut paths = Vec::new();
    let synth = dir.join("synthetic-split.pdf"); generate(&synth, 50, 100, 0); paths.push(("synthetic-text".to_string(), synth.clone()));
    if let Ok(list) = std::env::var("SM1_PERF_REAL") {
        for (index, line) in std::fs::read_to_string(list).unwrap().lines().enumerate() { if !line.trim().is_empty() && std::fs::metadata(line.trim()).map(|m| m.len() < 100 * 1_048_576).unwrap_or(false) { paths.push((format!("real-{:02}", index + 1), PathBuf::from(line.trim()))); } }
    }
    for (label, path) in paths {
        let bytes = std::fs::read(&path).unwrap();
        let Ok(doc) = pdfium.load_pdf_from_byte_vec(bytes, None) else { continue };
        let Ok(page) = doc.pages().get(0) else { continue };
        for width in [816, 1224, 1632] {
            let cfg = PdfRenderConfig::new().set_target_width(width).set_maximum_height(5000).render_annotations(true);
            let (bitmap, t_render) = time(|| page.render_with_config(&cfg));
            let Ok(bitmap) = bitmap else { continue };
            let (image, t_image) = time(|| bitmap.as_image().unwrap());
            let mut bytes = std::io::Cursor::new(Vec::new());
            let (_, t_png) = time(|| image.write_to(&mut bytes, image::ImageFormat::Png).unwrap());
            out(&format!("{label} split width={width} dims={}x{} pdfium_render_ms={t_render:.1} bitmap_to_image_ms={t_image:.1} png_encode_ms={t_png:.1} png_kib={:.1} raw_bgra_mib={:.1}", image.width(), image.height(), bytes.get_ref().len() as f64 / 1024.0, (image.width() * image.height() * 4) as f64 / 1_048_576.0));
        }
    }
    let _ = std::fs::remove_file(synth);
    // Page-size collection variants (what open does for the viewer's scroll layout).
    let mut variants: Vec<(String, PathBuf)> = Vec::new();
    for pages in [1000usize, 10000, 65536] { let path = dir.join(format!("synthetic-sizes-{pages}.pdf")); generate(&path, pages, 100, 0); variants.push((format!("synthetic-{pages}"), path)); }
    if let Ok(list) = std::env::var("SM1_PERF_REAL") { for (index, line) in std::fs::read_to_string(list).unwrap().lines().enumerate() { if !line.trim().is_empty() { variants.push((format!("real-{:02}", index + 1), PathBuf::from(line.trim()))); } } }
    {
        // Edge cases for the by-index query: /Rotate, CropBox inside MediaBox, offset origins, inherited boxes, UserUnit.
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let specs: [(&str, [i64; 4], Option<[i64; 4]>, Option<i64>, bool); 8] = [
            ("plain", [0, 0, 612, 792], None, None, false), ("rot90", [0, 0, 612, 792], None, Some(90), false), ("rot270", [0, 0, 300, 500], None, Some(270), false),
            ("crop", [0, 0, 612, 792], Some([50, 60, 400, 500]), None, false), ("crop_rot90", [0, 0, 612, 792], Some([50, 60, 400, 500]), Some(90), false),
            ("offset", [-100, -200, 512, 592], None, None, false), ("rot_neg90", [0, 0, 612, 792], None, Some(-90), false), ("userunit", [0, 0, 612, 792], None, None, true),
        ];
        let mut kids = Vec::new();
        for (_, media, crop, rotate, user_unit) in specs {
            let mut d = dictionary! { "Type" => "Page", "Parent" => pages_id, "MediaBox" => media.iter().map(|v| Object::from(*v)).collect::<Vec<_>>() };
            if let Some(c) = crop { d.set("CropBox", c.iter().map(|v| Object::from(*v)).collect::<Vec<_>>()); }
            if let Some(r) = rotate { d.set("Rotate", r); }
            if user_unit { d.set("UserUnit", 2.0f32); }
            kids.push(Object::Reference(doc.add_object(d)));
        }
        doc.objects.insert(pages_id, Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 8i64 }));
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let path = dir.join("synthetic-edge.pdf"); doc.save(&path).unwrap();
        let d = pdfium.load_pdf_from_file(&path, None).unwrap();
        let pages = d.pages();
        for (i, (name, ..)) in specs.iter().enumerate() {
            let p = pages.get(i as i32).unwrap(); let r = pages.page_size(i as i32).unwrap();
            out(&format!("edge {name} get=({:.2}x{:.2}) by_index=({:.2}x{:.2})", p.width().value, p.height().value, r.width().value, r.height().value));
        }
        let _ = std::fs::remove_file(&path);
    }
    for (label, path) in variants {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let (m0, p0, _) = memory();
        let (b, t_size, n, ws_b, pv_b) = {
            let doc = pdfium.load_pdf_from_byte_vec(bytes.clone(), None).unwrap();
            let pages = doc.pages(); let n = pages.len();
            let (b, t_size) = time(|| (0..n).map(|i| { let r = pages.page_size(i).unwrap(); (r.width().value, r.height().value) }).collect::<Vec<_>>());
            let (ws, pv, _) = memory();
            (b, t_size, n, ws, pv)
        };
        let (a, t_get, ws_a, pv_a) = {
            let doc = pdfium.load_pdf_from_byte_vec(bytes, None).unwrap();
            let pages = doc.pages();
            let (a, t_get) = time(|| (0..n).map(|i| { let p = pages.get(i).unwrap(); (p.width().value, p.height().value) }).collect::<Vec<_>>());
            let (ws, pv, _) = memory();
            (a, t_get, ws, pv)
        };
        let same = a.iter().zip(b.iter()).all(|(x, y)| (x.0 - y.0).abs() < 0.01 && (x.1 - y.1).abs() < 0.01);
        out(&format!("{label} page_sizes pages={n} get_page_loop_ms={t_get:.1} page_size_by_index_ms={t_size:.1} identical={same} mem_before_ws={m0:.0}/{p0:.0} mem_after_by_index_ws={ws_b:.0}/{pv_b:.0} mem_after_get_loop_ws={ws_a:.0}/{pv_a:.0}"));
        if label.starts_with("synthetic") { let _ = std::fs::remove_file(&path); }
    }
}
