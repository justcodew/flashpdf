//! Optional page rendering via PDFium (gated behind the `render` feature).
//!
//! PDFium is Google's BSD-licensed C++ PDF library (used in Chrome). We link
//! against it via the `pdfium-render` Rust crate, which performs dynamic FFI
//! to the PDFium C ABI at runtime. This means the PDFium dynamic library
//! (`.so` / `.dylib` / `.dll`) is **not** bundled with the crate — it must
//! be provided at runtime via:
//!
//! 1. `PDFIUM_PATH` environment variable pointing at the library file, OR
//! 2. `./pdfium-bin/libpdfium.{so,dylib,dll}` (dev convenience, gitignored), OR
//! 3. system library search path.
//!
//! See `docs/RENDERING.md` for download instructions.

use std::path::PathBuf;
use std::sync::Mutex;

use pdfium_render::prelude::*;

/// Process-wide PDFium instance. PDFium's C library can only be initialized
/// once per process (`FPDF_InitLibrary` is idempotent-but-exclusive). We
/// store the bound instance behind a `Mutex<Option<&'static Pdfium>>` —
/// the instance itself is leaked on first init (it lives for the process
/// lifetime anyway, exactly what we want).
static PDFIUM: Mutex<Option<&'static Pdfium>> = Mutex::new(None);

/// Lock the static, init it on first call (load + bind + leak), return a
/// `&'static Pdfium`. Subsequent calls skip the bind.
fn get_or_init_pdfium() -> Result<&'static Pdfium, String> {
    let mut guard = PDFIUM
        .lock()
        .map_err(|e| format!("pdfium mutex poisoned: {e}"))?;
    if let Some(p) = guard.as_ref() {
        return Ok(*p);
    }
    let pdfium = Box::leak(Box::new(load_pdfium()?));
    *guard = Some(pdfium);
    Ok(pdfium)
}

/// Render a single page of `path` to PNG bytes at the given DPI.
///
/// `dpi=72` is PDF native size; `dpi=150` is a reasonable screen preview.
///
/// Errors are returned as human-readable strings — rendering is an optional
/// leaf capability and callers only need to surface the message.
pub fn render_page_to_png(path: &str, page_idx: usize, dpi: u32) -> Result<Vec<u8>, String> {
    let pdfium = get_or_init_pdfium()?;
    let pdf = pdfium
        .load_pdf_from_file(path, None)
        .map_err(|e| format!("pdfium load failed for {path}: {e}"))?;

    let page_count = pdf.pages().len();
    if page_idx >= page_count as usize {
        return Err(format!(
            "page index {page_idx} out of range (pdf has {page_count} pages)"
        ));
    }

    let page = pdf
        .pages()
        .get(page_idx as i32)
        .map_err(|e| format!("page {page_idx} get: {e}"))?;

    let scale = dpi as f32 / 72.0;
    let config = PdfRenderConfig::new()
        .set_format(PdfBitmapFormat::BGRA)
        .set_clear_color(PdfColor::WHITE)
        .scale_page_by_factor(scale);

    let bitmap = page
        .render_with_config(&config)
        .map_err(|e| format!("render page {page_idx}: {e}"))?;

    let width = bitmap.width() as u32;
    let height = bitmap.height() as u32;
    let raw = bitmap.as_raw_bytes(); // BGRA, tightly packed

    // Convert BGRA -> RGBA in place.
    let mut rgba = raw;
    for chunk in rgba.chunks_exact_mut(4) {
        chunk.swap(0, 2);
    }

    let img = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "image buffer construction failed (size mismatch)".to_string())?;

    let mut out = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut out);
    image::ImageEncoder::write_image(
        encoder,
        &img,
        width,
        height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| format!("png encode: {e}"))?;

    Ok(out)
}

/// Locate and bind to the PDFium dynamic library.
///
/// Search order (first hit wins):
/// 1. `PDFIUM_PATH` env (exact file path)
/// 2. `./pdfium-bin/<platform_lib>` (dev convenience)
/// 3. system library search path
fn load_pdfium() -> Result<Pdfium, String> {
    // 1. PDFIUM_PATH env. Accept either the library file itself or the
    //    directory containing it — `pdfium_platform_library_name_at_path`
    //    expects a directory, so normalize file inputs to their parent.
    if let Ok(p) = std::env::var("PDFIUM_PATH") {
        let raw = PathBuf::from(&p);
        let dir = if raw.is_file() {
            raw.parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| raw.clone())
        } else {
            raw
        };
        return match Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)) {
            Ok(bindings) => Ok(Pdfium::new(bindings)),
            Err(e) => Err(format!("PDFIUM_PATH={p}: failed to bind ({e})")),
        };
    }

    // 2. ./pdfium-bin/ dev convenience
    for candidate in dev_candidates() {
        if let Ok(bindings) =
            Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&candidate))
        {
            return Ok(Pdfium::new(bindings));
        }
    }

    // 3. system library
    Pdfium::bind_to_system_library()
        .map(Pdfium::new)
        .map_err(|e| {
            format!(
                "PDFium dynamic library not found. Set PDFIUM_PATH=<path> or place \
                 libpdfium.{{so,dylib,dll}} under ./pdfium-bin/. \
                 Download from https://github.com/bblanchon/pdfium-binaries/releases — {e}"
            )
        })
}

#[cfg(target_os = "linux")]
fn dev_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from("pdfium-bin/libpdfium.so")]
}

#[cfg(target_os = "macos")]
fn dev_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from("pdfium-bin/libpdfium.dylib")]
}

#[cfg(target_os = "windows")]
fn dev_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from("pdfium-bin/pdfium.dll")]
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn dev_candidates() -> Vec<PathBuf> {
    vec![]
}

#[cfg(test)]
#[cfg(feature = "render")]
mod tests {
    use super::*;

    fn pdfium_available() -> bool {
        std::env::var("PDFIUM_PATH").is_ok() || std::path::Path::new("pdfium-bin").exists()
    }

    #[test]
    fn test_render_first_page_to_png() {
        if !pdfium_available() {
            eprintln!("skipping render test: PDFium binary not available");
            return;
        }
        let test_pdf = "test_data/sample.pdf";
        if !std::path::Path::new(test_pdf).exists() {
            eprintln!("skipping render test: {test_pdf} missing");
            return;
        }
        let png = render_page_to_png(test_pdf, 0, 72).expect("render should succeed");
        assert_eq!(&png[0..8], b"\x89PNG\r\n\x1a\n", "PNG magic header");
        assert!(png.len() > 100, "PNG should be non-trivial");
    }
}
