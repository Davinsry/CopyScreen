use crate::capture::CapturedFrame;
use crate::selection::{OcrLayout, RectF};
use std::ffi::{c_char, c_void, CStr, CString};
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use windows::core::{s, w, HSTRING, PCWSTR, PWSTR};
use windows::Globalization::Language;
use windows::Media::Ocr::OcrEngine;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};

const ONE_OCR_MODEL_KEY: &str = r#"kj)TGtrK>f]b[Piow.gU+nC@s""""""4"#;

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct RawOneOcrImage {
    t: i32,
    col: i32,
    row: i32,
    _unk: i32,
    step: i64,
    data_ptr: i64,
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
struct RawOneOcrBBox {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    x3: f32,
    y3: f32,
    x4: f32,
    y4: f32,
}

type FnCreateOcrInitOptions = unsafe extern "system" fn(*mut *mut c_void) -> i32;
type FnOcrInitOptionsSetUseModelDelayLoad = unsafe extern "system" fn(*mut c_void, u8) -> i32;
type FnCreateOcrPipeline =
    unsafe extern "system" fn(*const c_char, *const c_char, *mut c_void, *mut *mut c_void) -> i32;
type FnCreateOcrProcessOptions = unsafe extern "system" fn(*mut *mut c_void) -> i32;
type FnOcrProcessOptionsSetMaxRecognitionLineCount =
    unsafe extern "system" fn(*mut c_void, i32) -> i32;
type FnRunOcrPipeline = unsafe extern "system" fn(
    *mut c_void,
    *const RawOneOcrImage,
    *mut c_void,
    *mut *mut c_void,
) -> i32;
type FnGetOcrLineCount = unsafe extern "system" fn(*mut c_void, *mut i64) -> i32;
type FnGetOcrLine = unsafe extern "system" fn(*mut c_void, i64, *mut *mut c_void) -> i32;
type FnGetOcrLineWordCount = unsafe extern "system" fn(*mut c_void, *mut i64) -> i32;
type FnGetOcrWord = unsafe extern "system" fn(*mut c_void, i64, *mut *mut c_void) -> i32;
type FnGetOcrWordContent = unsafe extern "system" fn(*mut c_void, *mut *const c_char) -> i32;
type FnGetOcrWordBoundingBox =
    unsafe extern "system" fn(*mut c_void, *mut *const RawOneOcrBBox) -> i32;
type FnGetOcrWordConfidence = unsafe extern "system" fn(*mut c_void, *mut f32) -> i32;
type FnReleaseOcrResult = unsafe extern "system" fn(*mut c_void);

/// Pre-warmed singleton holding the initialized `oneocr.dll` pipeline in memory so
/// shortcut triggers execute `RunOcrPipeline` immediately with zero model-loading lag!
struct CachedOneOcrEngine {
    _hmod: isize,
    pipeline: isize,
    proc_opt: isize,
    run_pipeline: FnRunOcrPipeline,
    get_line_count: FnGetOcrLineCount,
    get_line: FnGetOcrLine,
    get_word_count: FnGetOcrLineWordCount,
    get_word: FnGetOcrWord,
    get_word_content: FnGetOcrWordContent,
    get_word_bbox: FnGetOcrWordBoundingBox,
    get_word_conf: FnGetOcrWordConfidence,
    release_result: FnReleaseOcrResult,
}

unsafe impl Send for CachedOneOcrEngine {}
unsafe impl Sync for CachedOneOcrEngine {}

static CACHED_ONEOCR: OnceLock<Option<Mutex<CachedOneOcrEngine>>> = OnceLock::new();

/// Warms up the OneOCR engine on a background thread at startup so the first shortcut press is instant.
pub fn warmup_ocr_engine() {
    let _ = get_cached_oneocr();
}

fn get_cached_oneocr() -> Option<&'static Mutex<CachedOneOcrEngine>> {
    CACHED_ONEOCR
        .get_or_init(|| match init_oneocr_engine() {
            Ok(eng) => Some(Mutex::new(eng)),
            Err(_) => None,
        })
        .as_ref()
}

fn find_system_oneocr_paths() -> Option<(PathBuf, PathBuf)> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let dll = dir.join("oneocr.dll");
            let model = dir.join("oneocr.onemodel");
            if dll.is_file() && model.is_file() {
                return Some((dll, model));
            }
        }
    }

    // Check if already staged in %LOCALAPPDATA%\CopyScreen\oneocr
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let cache_dir = local_app_data.join("CopyScreen").join("oneocr");
        let dst_dll = cache_dir.join("oneocr.dll");
        let dst_model = cache_dir.join("oneocr.onemodel");
        if dst_dll.is_file() && dst_model.is_file() {
            return Some((dst_dll, dst_model));
        }
    }

    unsafe {
        let packages_subkey =
            w!("Software\\Classes\\Local Settings\\Software\\Microsoft\\Windows\\CurrentVersion\\AppModel\\Repository\\Packages");
        let mut hkey_pkgs = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, packages_subkey, 0, KEY_READ, &mut hkey_pkgs).is_err() {
            return None;
        }

        let mut index = 0u32;
        let mut found: Option<(PathBuf, PathBuf)> = None;

        loop {
            let mut name_buf = [0u16; 260];
            let mut name_len = name_buf.len() as u32;
            let res = RegEnumKeyExW(
                hkey_pkgs,
                index,
                PWSTR(name_buf.as_mut_ptr()),
                &mut name_len,
                None,
                PWSTR::null(),
                None,
                None,
            );
            if res.is_err() {
                break;
            }
            index += 1;

            let pkg_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
            if !pkg_name.starts_with("Microsoft.ScreenSketch_") {
                continue;
            }

            let mut hkey_pkg = HKEY::default();
            if RegOpenKeyExW(
                hkey_pkgs,
                PCWSTR(name_buf.as_ptr()),
                0,
                KEY_READ,
                &mut hkey_pkg,
            )
            .is_ok()
            {
                let mut root_buf = [0u16; 512];
                let mut byte_len = (root_buf.len() * size_of::<u16>()) as u32;
                if RegQueryValueExW(
                    hkey_pkg,
                    w!("PackageRootFolder"),
                    None,
                    None,
                    Some(root_buf.as_mut_ptr() as *mut u8),
                    Some(&mut byte_len),
                )
                .is_ok()
                {
                    let u16_len = (byte_len as usize / 2).saturating_sub(1);
                    let root_str =
                        String::from_utf16_lossy(&root_buf[..u16_len.min(root_buf.len())]);
                    let root_path = PathBuf::from(root_str.trim_end_matches('\0'));
                    let snip_dir = root_path.join("SnippingTool");
                    let src_dll = snip_dir.join("oneocr.dll");
                    let src_model = snip_dir.join("oneocr.onemodel");
                    let src_ort = snip_dir.join("onnxruntime.dll");
                    if src_dll.is_file() && src_model.is_file() {
                        found = stage_oneocr_to_local_cache(&src_dll, &src_model, &src_ort);
                    }
                }
                let _ = RegCloseKey(hkey_pkg);
            }

            if found.is_some() {
                break;
            }
        }

        let _ = RegCloseKey(hkey_pkgs);
        found
    }
}

fn stage_oneocr_to_local_cache(
    src_dll: &PathBuf,
    src_model: &PathBuf,
    src_ort: &PathBuf,
) -> Option<(PathBuf, PathBuf)> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::temp_dir().into())?;
    let cache_dir = local_app_data.join("CopyScreen").join("oneocr");
    let _ = std::fs::create_dir_all(&cache_dir);

    let dst_dll = cache_dir.join("oneocr.dll");
    let dst_model = cache_dir.join("oneocr.onemodel");
    let dst_ort = cache_dir.join("onnxruntime.dll");

    if !dst_dll.is_file() {
        std::fs::copy(src_dll, &dst_dll).ok()?;
    }
    if !dst_model.is_file() {
        std::fs::copy(src_model, &dst_model).ok()?;
    }
    if !dst_ort.is_file() && src_ort.is_file() {
        let _ = std::fs::copy(src_ort, &dst_ort);
    }

    if dst_dll.is_file() && dst_model.is_file() {
        Some((dst_dll, dst_model))
    } else {
        None
    }
}

fn init_oneocr_engine() -> Result<CachedOneOcrEngine, String> {
    let (dll_path, model_path) =
        find_system_oneocr_paths().ok_or_else(|| "OneOCR tidak ditemukan.".to_string())?;

    let dll_utf16: Vec<u16> = dll_path
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let model_cstr = CString::new(model_path.to_string_lossy().as_bytes())
        .map_err(|_| "Path model OneOCR tidak valid.".to_string())?;
    let key_cstr = CString::new(ONE_OCR_MODEL_KEY).unwrap();

    unsafe {
        let hmod: HMODULE = LoadLibraryExW(
            PCWSTR(dll_utf16.as_ptr()),
            None,
            LOAD_WITH_ALTERED_SEARCH_PATH,
        )
        .map_err(|e| format!("Gagal memuat oneocr.dll: {e}"))?;

        let create_init: FnCreateOcrInitOptions = std::mem::transmute(
            GetProcAddress(hmod, s!("CreateOcrInitOptions")).ok_or("Missing CreateOcrInitOptions")?,
        );
        let set_delay: FnOcrInitOptionsSetUseModelDelayLoad = std::mem::transmute(
            GetProcAddress(hmod, s!("OcrInitOptionsSetUseModelDelayLoad"))
                .ok_or("Missing OcrInitOptionsSetUseModelDelayLoad")?,
        );
        let create_pipeline: FnCreateOcrPipeline = std::mem::transmute(
            GetProcAddress(hmod, s!("CreateOcrPipeline")).ok_or("Missing CreateOcrPipeline")?,
        );
        let create_proc_opt: FnCreateOcrProcessOptions = std::mem::transmute(
            GetProcAddress(hmod, s!("CreateOcrProcessOptions"))
                .ok_or("Missing CreateOcrProcessOptions")?,
        );
        let set_max_lines: FnOcrProcessOptionsSetMaxRecognitionLineCount = std::mem::transmute(
            GetProcAddress(hmod, s!("OcrProcessOptionsSetMaxRecognitionLineCount"))
                .ok_or("Missing OcrProcessOptionsSetMaxRecognitionLineCount")?,
        );
        let run_pipeline: FnRunOcrPipeline = std::mem::transmute(
            GetProcAddress(hmod, s!("RunOcrPipeline")).ok_or("Missing RunOcrPipeline")?,
        );
        let get_line_count: FnGetOcrLineCount = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrLineCount")).ok_or("Missing GetOcrLineCount")?,
        );
        let get_line: FnGetOcrLine = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrLine")).ok_or("Missing GetOcrLine")?,
        );
        let get_word_count: FnGetOcrLineWordCount = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrLineWordCount")).ok_or("Missing GetOcrLineWordCount")?,
        );
        let get_word: FnGetOcrWord = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrWord")).ok_or("Missing GetOcrWord")?,
        );
        let get_word_content: FnGetOcrWordContent = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrWordContent")).ok_or("Missing GetOcrWordContent")?,
        );
        let get_word_bbox: FnGetOcrWordBoundingBox = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrWordBoundingBox"))
                .ok_or("Missing GetOcrWordBoundingBox")?,
        );
        let get_word_conf: FnGetOcrWordConfidence = std::mem::transmute(
            GetProcAddress(hmod, s!("GetOcrWordConfidence"))
                .ok_or("Missing GetOcrWordConfidence")?,
        );
        let release_result: FnReleaseOcrResult = std::mem::transmute(
            GetProcAddress(hmod, s!("ReleaseOcrResult")).ok_or("Missing ReleaseOcrResult")?,
        );

        let mut init_opt: *mut c_void = ptr::null_mut();
        if create_init(&mut init_opt) != 0 {
            return Err("CreateOcrInitOptions gagal.".to_string());
        }
        let _ = set_delay(init_opt, 0);

        let mut pipeline: *mut c_void = ptr::null_mut();
        if create_pipeline(
            model_cstr.as_ptr(),
            key_cstr.as_ptr(),
            init_opt,
            &mut pipeline,
        ) != 0
        {
            return Err("CreateOcrPipeline gagal.".to_string());
        }

        let mut proc_opt: *mut c_void = ptr::null_mut();
        if create_proc_opt(&mut proc_opt) != 0 {
            return Err("CreateOcrProcessOptions gagal.".to_string());
        }
        let _ = set_max_lines(proc_opt, 1000);

        Ok(CachedOneOcrEngine {
            _hmod: hmod.0 as isize,
            pipeline: pipeline as isize,
            proc_opt: proc_opt as isize,
            run_pipeline,
            get_line_count,
            get_line,
            get_word_count,
            get_word,
            get_word_content,
            get_word_bbox,
            get_word_conf,
            release_result,
        })
    }
}

fn recognize_with_cached_oneocr(
    frame: &CapturedFrame,
) -> Result<Vec<Vec<(String, RectF)>>, String> {
    let mutex = get_cached_oneocr().ok_or_else(|| "OneOCR tidak tersedia.".to_string())?;
    let eng = mutex
        .lock()
        .map_err(|_| "Mutex OneOCR terkunci.".to_string())?;

    unsafe {
        let raw_img = RawOneOcrImage {
            t: 3,
            col: frame.width as i32,
            row: frame.height as i32,
            _unk: 0,
            step: (frame.width * 4) as i64,
            data_ptr: frame.pixels_bgra.as_ptr() as i64,
        };

        let mut ocr_res: *mut c_void = ptr::null_mut();
        let code = (eng.run_pipeline)(
            eng.pipeline as *mut c_void,
            &raw_img,
            eng.proc_opt as *mut c_void,
            &mut ocr_res,
        );
        if code != 0 || ocr_res.is_null() {
            return Err(format!("RunOcrPipeline gagal (code {code})."));
        }

        let mut line_count: i64 = 0;
        let _ = (eng.get_line_count)(ocr_res, &mut line_count);
        let mut raw_lines = Vec::with_capacity(line_count.max(0) as usize);

        for i in 0..line_count {
            let mut line_ptr: *mut c_void = ptr::null_mut();
            if (eng.get_line)(ocr_res, i, &mut line_ptr) != 0 || line_ptr.is_null() {
                continue;
            }

            let mut word_count: i64 = 0;
            if (eng.get_word_count)(line_ptr, &mut word_count) != 0 || word_count <= 0 {
                continue;
            }

            let mut line_words = Vec::with_capacity(word_count as usize);
            for j in 0..word_count {
                let mut word_ptr: *mut c_void = ptr::null_mut();
                if (eng.get_word)(line_ptr, j, &mut word_ptr) != 0 || word_ptr.is_null() {
                    continue;
                }

                let mut content_ptr: *const c_char = ptr::null();
                if (eng.get_word_content)(word_ptr, &mut content_ptr) != 0 || content_ptr.is_null()
                {
                    continue;
                }
                let text = CStr::from_ptr(content_ptr).to_string_lossy().to_string();
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }

                let mut confidence: f32 = 1.0;
                let _ = (eng.get_word_conf)(word_ptr, &mut confidence);

                let mut bbox_ptr: *const RawOneOcrBBox = ptr::null();
                if (eng.get_word_bbox)(word_ptr, &mut bbox_ptr) != 0 || bbox_ptr.is_null() {
                    continue;
                }
                let b = ptr::read_unaligned(bbox_ptr);
                let min_x = b.x1.min(b.x2).min(b.x3).min(b.x4);
                let max_x = b.x1.max(b.x2).max(b.x3).max(b.x4);
                let min_y = b.y1.min(b.y2).min(b.y3).min(b.y4);
                let max_y = b.y1.max(b.y2).max(b.y3).max(b.y4);
                let bw = max_x - min_x;
                let bh = max_y - min_y;

                if bw <= 1.0 || bh <= 1.0 {
                    continue;
                }

                let char_count = trimmed.chars().count();
                if is_likely_icon_badge(trimmed, char_count, bw, bh, confidence, word_count as u32)
                {
                    continue;
                }

                line_words.push((text, RectF::new(min_x, min_y, bw, bh)));
            }

            let filtered = filter_isolated_icon_glyphs(line_words);
            if !filtered.is_empty() {
                raw_lines.push(filtered);
            }
        }

        (eng.release_result)(ocr_res);
        Ok(raw_lines)
    }
}

/// Narrowly filters ONLY decorative badges inside desktop/file icons (such as standalone `'W'` on a Word icon,
/// `'X'` on an Excel icon, or `'PDF'` stamp inside a PDF icon) while preserving 100% of real UI words,
/// headings ("About", "Public", "Sponsor"), version numbers ("3.9.0.787"), and dotted identifiers!
fn is_likely_icon_badge(
    trimmed: &str,
    char_count: usize,
    _bw: f32,
    bh: f32,
    confidence: f32,
    line_word_count: u32,
) -> bool {
    if char_count == 1 {
        let ch = trimmed.chars().next().unwrap_or(' ');
        if !ch.is_alphanumeric() {
            return true;
        }
        // Only reject standalone single-letter icon badges ('W', 'X', 'P') when they sit alone
        // with icon-badge height (> 22px) or low confidence
        if line_word_count == 1 && (bh > 21.0 || confidence < 0.80) {
            return true;
        }
    }

    // Reject isolated 'PDF' / 'DOC' / 'XLS' stamp inside File Explorer document icons
    let upper = trimmed.to_ascii_uppercase();
    if matches!(upper.as_str(), "PDF" | "DOC" | "XLS" | "PPT")
        && line_word_count == 1
        && bh <= 18.0
        && confidence < 0.92
    {
        return true;
    }

    // Reject huge graphic bounding boxes
    if bh > 72.0 && char_count <= 3 {
        return true;
    }

    false
}

/// Removes single-character icon badges (like a row of 'W', 'W', 'X' on File Explorer icons)
/// that sit isolated far away (> 2.2x height) from any neighboring word on the same line,
/// while preserving all multi-character words ("About", "Public", "SafeExamBrowser.Client", etc.)!
fn filter_isolated_icon_glyphs(words: Vec<(String, RectF)>) -> Vec<(String, RectF)> {
    if words.len() <= 1 {
        return words;
    }

    let mut out = Vec::with_capacity(words.len());
    for (i, (text, rect)) in words.iter().enumerate() {
        let chars = text.trim().chars().count();
        if chars == 1 {
            let max_neighbor_dist = (rect.height * 2.2).clamp(16.0, 42.0);
            let has_close_left =
                i > 0 && (rect.x - words[i - 1].1.right()).abs() <= max_neighbor_dist;
            let has_close_right = i + 1 < words.len()
                && (words[i + 1].1.x - rect.right()).abs() <= max_neighbor_dist;
            if !has_close_left && !has_close_right {
                continue;
            }
        }
        out.push((text.clone(), *rect));
    }
    out
}

/// Fallback Windows.Media.Ocr engine for Windows 10 systems without OneOCR.
pub fn create_ocr_engine() -> Result<OcrEngine, String> {
    if let Ok(engine) = OcrEngine::TryCreateFromUserProfileLanguages() {
        return Ok(engine);
    }

    for tag in ["en-US", "en", "id-ID", "id"] {
        if let Ok(lang) = Language::CreateLanguage(&HSTRING::from(tag)) {
            if OcrEngine::IsLanguageSupported(&lang).unwrap_or(false) {
                if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang) {
                    return Ok(engine);
                }
            }
        }
    }

    if let Ok(langs) = OcrEngine::AvailableRecognizerLanguages() {
        if langs.Size().unwrap_or(0) > 0 {
            if let Ok(first_lang) = langs.GetAt(0) {
                if let Ok(engine) = OcrEngine::TryCreateFromLanguage(&first_lang) {
                    return Ok(engine);
                }
            }
        }
    }

    Err("Windows OCR Engine tidak tersedia.".to_string())
}

/// Recognizes all text on screen using the pre-warmed Windows 11 OneOCR neural engine
/// (falling back to `Windows.Media.Ocr` only if OneOCR is not present on the OS).
pub fn recognize_frame(frame: &CapturedFrame) -> Result<OcrLayout, String> {
    if let Ok(lines) = recognize_with_cached_oneocr(frame) {
        return Ok(OcrLayout::from_raw_lines(lines));
    }

    let max_dim = OcrEngine::MaxImageDimension().unwrap_or(4096);
    if frame.width > max_dim || frame.height > max_dim {
        return Err(format!(
            "Resolusi tangkapan layar ({}x{}) melebihi batas maksimum Windows OCR ({}px).",
            frame.width, frame.height, max_dim
        ));
    }

    let bitmap = frame.to_software_bitmap()?;
    let engine = create_ocr_engine()?;
    let ocr_result = engine
        .RecognizeAsync(&bitmap)
        .map_err(|e| format!("Gagal memulai proses OCR: {e}"))?
        .get()
        .map_err(|e| format!("Gagal menyelesaikan pengenalan teks OCR: {e}"))?;

    let lines = ocr_result
        .Lines()
        .map_err(|e| format!("Gagal membaca baris hasil OCR: {e}"))?;

    let line_count = lines.Size().unwrap_or(0);
    let mut raw_lines = Vec::with_capacity(line_count as usize);

    for i in 0..line_count {
        let Ok(line) = lines.GetAt(i) else {
            continue;
        };
        let Ok(words) = line.Words() else {
            continue;
        };
        let word_count = words.Size().unwrap_or(0);
        if word_count == 0 {
            continue;
        }
        let mut line_words = Vec::with_capacity(word_count as usize);

        for j in 0..word_count {
            let Ok(word) = words.GetAt(j) else {
                continue;
            };
            let Ok(text_hstr) = word.Text() else {
                continue;
            };
            let Ok(rect) = word.BoundingRect() else {
                continue;
            };
            let text = text_hstr.to_string();
            let trimmed = text.trim();
            if trimmed.is_empty() || rect.Width <= 1.0 || rect.Height <= 1.0 {
                continue;
            }
            let char_count = trimmed.chars().count();
            if is_likely_icon_badge(trimmed, char_count, rect.Width, rect.Height, 0.90, word_count)
            {
                continue;
            }
            line_words.push((text, RectF::new(rect.X, rect.Y, rect.Width, rect.Height)));
        }

        let filtered = filter_isolated_icon_glyphs(line_words);
        if !filtered.is_empty() {
            raw_lines.push(filtered);
        }
    }

    Ok(OcrLayout::from_raw_lines(raw_lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn initializes_cached_oneocr_and_processes_blank_frame_fast() {
        warmup_ocr_engine();

        let blank_frame = CapturedFrame {
            monitor_x: 0,
            monitor_y: 0,
            width: 120,
            height: 60,
            local_click_x: 60.0,
            local_click_y: 30.0,
            pixels_bgra: vec![255u8; 120 * 60 * 4],
        };
        let t0 = Instant::now();
        let layout = recognize_frame(&blank_frame).expect("OCR on blank frame should succeed");
        let elapsed_ms = t0.elapsed().as_millis();
        println!("Cached OneOCR blank frame took: {elapsed_ms} ms");
        assert!(layout.is_empty());
    }

    #[test]
    fn filters_isolated_icon_badges_but_preserves_sentence_words() {
        assert!(is_likely_icon_badge("W", 1, 22.0, 24.0, 0.90, 1));
        assert!(is_likely_icon_badge("|", 1, 8.0, 95.0, 0.99, 4));
        assert!(is_likely_icon_badge("PDF", 3, 24.0, 14.0, 0.85, 1));
        assert!(!is_likely_icon_badge("About", 5, 45.0, 18.0, 0.99, 1));
        assert!(!is_likely_icon_badge(
            "SafeExamBrowser.Applications.Contracts",
            38,
            240.0,
            16.0,
            0.95,
            1
        ));

        let icon_row = vec![
            ("W".to_string(), RectF::new(50.0, 100.0, 20.0, 22.0)),
            ("W".to_string(), RectF::new(190.0, 100.0, 20.0, 22.0)),
            ("X".to_string(), RectF::new(330.0, 100.0, 20.0, 22.0)),
        ];
        assert!(filter_isolated_icon_glyphs(icon_row).is_empty());

        let sentence_row = vec![
            ("Create".to_string(), RectF::new(50.0, 100.0, 48.0, 18.0)),
            ("a".to_string(), RectF::new(104.0, 100.0, 10.0, 18.0)),
            ("project".to_string(), RectF::new(120.0, 100.0, 52.0, 18.0)),
        ];
        let kept = filter_isolated_icon_glyphs(sentence_row);
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn detects_github_dark_mode_dotted_identifiers_and_labels() {
        use std::mem::size_of;
        use windows::core::w;
        use windows::Win32::Foundation::{COLORREF, RECT};
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC,
            DeleteObject, FillRect, SelectObject, SetBkMode, SetTextColor, TextOutW, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_QUALITY,
            DIB_RGB_COLORS, FF_DONTCARE, FW_NORMAL, HDC, OUT_DEFAULT_PRECIS, TRANSPARENT,
        };

        warmup_ocr_engine();

        let width = 640u32;
        let height = 160u32;

        let pixels_bgra = unsafe {
            let hdc = CreateCompatibleDC(HDC::default());
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: width * height * 4,
                    ..Default::default()
                },
                bmiColors: Default::default(),
            };

            let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let hbm = CreateDIBSection(hdc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
                .expect("CreateDIBSection should succeed");
            let old_bm = SelectObject(hdc, hbm);

            // GitHub dark mode #0D1117 background
            let dark_brush = CreateSolidBrush(COLORREF(0x17110D));
            let rect = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            let _ = FillRect(hdc, &rect, dark_brush);
            let _ = DeleteObject(dark_brush);

            let hfont = CreateFontW(
                14,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                DEFAULT_QUALITY.0 as u32,
                FF_DONTCARE.0 as u32,
                w!("Segoe UI"),
            );
            let old_font = SelectObject(hdc, hfont);
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, COLORREF(0xF0E6E0));

            let l1: Vec<u16> = "SafeExamBrowser.Applications.Contracts"
                .encode_utf16()
                .collect();
            let _ = TextOutW(hdc, 24, 24, &l1);

            let l2: Vec<u16> = "SafeExamBrowser.Client".encode_utf16().collect();
            let _ = TextOutW(hdc, 24, 64, &l2);

            let _ = SetTextColor(hdc, COLORREF(0x978D84));
            let l3: Vec<u16> = "About   Public   Sponsor   Watch 5   3.9.0.787"
                .encode_utf16()
                .collect();
            let _ = TextOutW(hdc, 24, 104, &l3);

            let slice =
                std::slice::from_raw_parts(bits_ptr as *const u8, (width * height * 4) as usize);
            let mut vec = slice.to_vec();
            for px in vec.chunks_exact_mut(4) {
                px[3] = 255;
            }

            let _ = SelectObject(hdc, old_font);
            let _ = DeleteObject(hfont);
            let _ = SelectObject(hdc, old_bm);
            let _ = DeleteObject(hbm);
            let _ = DeleteDC(hdc);
            vec
        };

        let frame = CapturedFrame {
            monitor_x: 0,
            monitor_y: 0,
            width,
            height,
            local_click_x: 20.0,
            local_click_y: 40.0,
            pixels_bgra,
        };

        let t0 = Instant::now();
        let layout = recognize_frame(&frame).expect("Cached OneOCR should succeed");
        let elapsed_ms = t0.elapsed().as_millis();
        let full = layout.full_text();
        println!("Cached OneOCR GitHub dark mode took {elapsed_ms} ms:\n{full}");
        assert!(full.contains("Contracts") && full.contains("Client") && full.contains("About"));
    }
}
