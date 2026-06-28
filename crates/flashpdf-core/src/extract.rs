/// High-level extraction API: ties together document parsing, content stream
/// scanning, layout clustering, and image extraction into a single call.
use crate::document::Document;
use crate::image::{resolve_images, ExtractedImage};
use crate::layout::cluster_chars;
use crate::parser::content_stream::XObjectData;
use crate::parser::ParseResult;
use crate::parser::TextBlock;
use crate::types::PdfObject;
use rayon::prelude::*;
use std::collections::HashMap;

/// Options for extraction.
#[derive(Debug, Clone)]
pub struct ExtractOptions {
    pub page_parallel: bool,
    pub file_parallel: bool,
    pub include_images: bool,
    pub gpu: bool,
    /// Batch size for large documents (0 = no batching).
    pub batch_size: usize,
    /// When false (default), chars emitted under a non-axis-aligned text
    /// matrix are dropped before clustering. This preserves baseline
    /// accuracy on ordinary documents because XY-cut reading order can't
    /// mix horizontal and vertical text. Set to true to keep rotated text
    /// (arXiv watermarks, vertical chart labels) — they will be clustered
    /// and appended to the page's block list, but layout/reading order on
    /// mixed-orientation pages may degrade.
    pub include_rotated: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            page_parallel: true,
            file_parallel: true,
            include_images: true,
            gpu: false,
            batch_size: 50,
            include_rotated: false,
        }
    }
}

/// Result of extracting a single page.
#[derive(Debug, Clone)]
pub struct PageResult {
    pub blocks: Vec<crate::parser::TextBlock>,
    pub images: Vec<ExtractedImage>,
    /// True if this page looks like a scanned page: very little extractable
    /// text AND at least one image covering most of the page. Such pages
    /// require OCR to recover text; flashpdf returns the raw image bytes
    /// in `images` for the caller to feed into their OCR engine of choice.
    pub is_scanned: bool,
    /// Page rectangle [x0, y0, x1, y1] from /MediaBox (or union-bbox fallback).
    /// Exposed as `page.rect` in the fitz-style API.
    pub rect: [f64; 4],
    /// Counts of "dropped / suspicious" content the user may want to know
    /// about. Populated regardless of `include_rotated` — detection always
    /// runs, output policy is the user's choice via the option flags.
    pub diagnostics: PageDiagnostics,
    /// Hyperlinks on this page (URI / Goto / Named / Launch / GotoR).
    /// Empty for pages without `/Annots` or non-Link annotations.
    pub links: Vec<crate::links::PageLink>,
}

/// Per-page diagnostics: counts of content that was dropped or potentially
/// mis-decoded. Exposed as `page.diagnostics` in Python. Non-zero counts
/// tell the user "flashpdf found N items it couldn't faithfully extract";
/// the user can then decide whether to re-extract with different flags
/// (e.g. `include_rotated=True`) or feed the page to an OCR pipeline.
#[derive(Debug, Clone, Default)]
pub struct PageDiagnostics {
    /// Chars emitted under a non-axis-aligned text matrix (rotated/sheared
    /// text: arXiv sidebars, vertical chart labels). Dropped by default
    /// because XY-cut can't mix orientations; pass `include_rotated=True`
    /// to keep them as appended blocks.
    pub rotated_char_count: usize,
    /// Chars emitted under a /Type3 font. Type 3 glyphs are defined by
    /// drawing operators, not outlines — flashpdf decodes via /Widths +
    /// ToUnicode when available but positioning may be off and glyphs
    /// without ToUnicode are unreadable.
    pub type3_char_count: usize,
    /// Bytes that could not be mapped to Unicode and were emitted as
    /// U+FFFD. Indicates missing /ToUnicode or /Encoding on the font.
    pub undecoded_byte_count: usize,
    /// Blocks dropped by the reading-order margin filter (bbox extends
    /// more than 10% outside the page rect). Usually a symptom of
    /// mis-clustered vector graphics or rotated text whose AABB pokes
    /// outside the page.
    pub out_of_page_block_count: usize,
    /// Inline images encountered (BI/ID/EI operators, PDF spec §8.9.7).
    /// Inline images are pixel data embedded directly in the content stream
    /// rather than referenced via /XObject. They're now included in
    /// `page.get_images()` with `name="inline"`, and counted separately here
    /// so callers can detect "all images are inline" (common for old scans).
    pub inline_image_count: usize,
}

/// Thresholds for the scan-detection heuristic.
const SCAN_MIN_TEXT_CHARS: usize = 50;
const SCAN_MIN_IMAGE_AREA_FRAC: f64 = 0.7;

/// Decide whether a page is scanned based on text char count and the
/// largest image's bbox coverage of the page rect.
fn detect_scanned(
    blocks: &[TextBlock],
    images: &[crate::parser::content_stream::ImageRef],
    page_rect: &[f64; 4],
) -> bool {
    let char_count: usize = blocks
        .iter()
        .flat_map(|b| b.lines.iter())
        .flat_map(|l| l.spans.iter())
        .map(|s| s.text.chars().count())
        .sum();
    if char_count >= SCAN_MIN_TEXT_CHARS {
        return false;
    }
    let page_w = (page_rect[2] - page_rect[0]).max(1.0);
    let page_h = (page_rect[3] - page_rect[1]).max(1.0);
    let page_area = page_w * page_h;
    let max_img_area = images
        .iter()
        .map(|img| {
            let w = (img.bbox[2] - img.bbox[0]).max(0.0);
            let h = (img.bbox[3] - img.bbox[1]).max(0.0);
            w * h
        })
        .fold(0.0_f64, f64::max);
    max_img_area / page_area >= SCAN_MIN_IMAGE_AREA_FRAC
}

/// Result of extracting a document.
#[derive(Debug)]
pub struct ExtractResult {
    pub pages: Vec<PageResult>,
    /// Document-level metadata from `/Info`. Always populated; defaults to
    /// all-`None` fields when the document has no `/Info` entry.
    pub metadata: crate::document::DocumentMetadata,
    /// PDF version string from `%PDF-X.Y` header (e.g. `"1.7"`).
    pub pdf_version: Option<String>,
    /// True iff the PDF was encrypted with `/Standard` RC4 or AES-128 and
    /// flashpdf successfully decrypted it (empty user password path).
    pub is_encrypted: bool,
    /// True iff the PDF is linearized (`/Linearized 1` in first object,
    /// PDF spec §F.2). Informational only — extraction is identical.
    pub is_linearized: bool,
}

/// Extract text blocks and images from a single PDF file.
pub fn extract(path: &str, options: &ExtractOptions) -> ParseResult<ExtractResult> {
    let doc = Document::open(path)?;
    extract_doc(&doc, options)
}

/// Extract from an already-opened Document.
pub fn extract_doc(doc: &Document, options: &ExtractOptions) -> ParseResult<ExtractResult> {
    let span = tracing::span!(
        tracing::Level::DEBUG,
        "extract_doc",
        page_count_guess = tracing::field::Empty,
    );
    let _enter = span.enter();
    tracing::debug!(version = ?doc.pdf_version(), encrypted = doc.is_encrypted(), "starting extract");
    // Try the spec-compliant /Pages /Kids walk first. If the page tree is
    // broken (dangling /Pages ref, missing /Kids — common in Word/Office
    // exports and bug-regression PDFs), fall back to scanning every xref
    // entry for /Type /Page objects. Recovery never fails — if it finds
    // zero pages, we proceed with an empty doc rather than fataling.
    let page_refs = match doc.page_refs() {
        Ok(refs) if !refs.is_empty() => refs,
        _ => doc.recover_page_refs(),
    };
    let mmap_data: &[u8] = unsafe { std::mem::transmute(doc.mmap_slice()) };

    // Auto-batch: if page count > batch_size and batch_size > 0, process in batches.
    // Guard against page_refs.len()==0 — both recovery paths can return empty
    // for utterly broken PDFs, and `chunks(0)` panics.
    if page_refs.is_empty() {
        return Ok(ExtractResult {
            pages: Vec::new(),
            metadata: doc.metadata(),
            pdf_version: doc.pdf_version().map(|s| s.to_string()),
            is_encrypted: doc.is_encrypted(),
            is_linearized: doc.is_linearized(),
        });
    }
    let batch_size = if options.batch_size > 0 && page_refs.len() > options.batch_size {
        options.batch_size
    } else {
        page_refs.len()
    };

    let mut all_pages = Vec::with_capacity(page_refs.len());

    let mut batch_start = 0u32;
    for batch in page_refs.chunks(batch_size) {
        let batch_results = extract_page_batch(doc, batch, mmap_data, options, batch_start);
        batch_start = batch_start.saturating_add(batch.len() as u32);
        all_pages.extend(batch_results);
    }

    Ok(ExtractResult {
        pages: all_pages,
        metadata: doc.metadata(),
        pdf_version: doc.pdf_version().map(|s| s.to_string()),
        is_encrypted: doc.is_encrypted(),
        is_linearized: doc.is_linearized(),
    })
}

/// Extract a batch of pages (used for large document batching).
/// `batch_start_idx` is the global 0-based index of the first page in this
/// batch — needed so each PageResult can record its real page number for
/// link extraction and PyPage.number reporting.
fn extract_page_batch(
    doc: &Document,
    page_refs: &[crate::types::ObjectId],
    mmap_data: &[u8],
    options: &ExtractOptions,
    batch_start_idx: u32,
) -> Vec<PageResult> {
    if options.page_parallel && page_refs.len() > 1 {
        page_refs
            .par_iter()
            .enumerate()
            .map(|(i, r)| {
                extract_single_page(doc, *r, mmap_data, options, batch_start_idx + i as u32)
                    .unwrap_or_else(|_| PageResult {
                        blocks: vec![],
                        images: vec![],
                        is_scanned: false,
                        rect: [0.0, 0.0, 612.0, 792.0],
                        diagnostics: PageDiagnostics::default(),
                        links: vec![],
                    })
            })
            .collect()
    } else {
        page_refs
            .iter()
            .enumerate()
            .map(|(i, r)| {
                extract_single_page(doc, *r, mmap_data, options, batch_start_idx + i as u32)
                    .unwrap_or_else(|_| PageResult {
                        blocks: vec![],
                        images: vec![],
                        is_scanned: false,
                        rect: [0.0, 0.0, 612.0, 792.0],
                        diagnostics: PageDiagnostics::default(),
                        links: vec![],
                    })
            })
            .collect()
    }
}

/// Extract text blocks and images from multiple PDF files.
/// Supports file-level parallelism and async prefetch.
pub fn extract_many(
    paths: &[&str],
    options: &ExtractOptions,
) -> Vec<(String, ParseResult<ExtractResult>)> {
    if paths.len() <= 1 {
        return paths
            .iter()
            .map(|&path| (path.to_string(), extract(path, options)))
            .collect();
    }

    // For small file counts, sequential + prefetch is faster than rayon
    // due to thread pool overhead. Use rayon only for large batches.
    if options.file_parallel && paths.len() >= 8 {
        // File-level parallel via rayon for large batches
        paths
            .par_iter()
            .map(|&path| {
                let result = extract(path, options);
                (path.to_string(), result)
            })
            .collect()
    } else {
        // Sequential with prefetch: open next file while processing current
        let mut results = Vec::with_capacity(paths.len());

        for i in 0..paths.len() {
            // Start prefetching next file in background
            let prefetch_handle = if i + 1 < paths.len() {
                let next_path = paths[i + 1].to_string();
                Some(std::thread::spawn(move || Document::open(&next_path)))
            } else {
                None
            };

            // Process current file
            let result = extract(paths[i], options);
            results.push((paths[i].to_string(), result));

            // If prefetch completed, the next iteration will benefit from cached mmap
            if let Some(handle) = prefetch_handle {
                let _ = handle.join();
            }
        }

        results
    }
}

/// Resolve the page rectangle [x0, y0, x1, y1] from MediaBox, falling back
/// to the union bbox of all text blocks if MediaBox is missing or malformed.
fn page_rect(page: &PdfObject<'_>, blocks: &[TextBlock]) -> [f64; 4] {
    if let Some(arr) = page.get(b"MediaBox").and_then(PdfObject::as_array) {
        if arr.len() == 4 {
            let mut r = [0.0; 4];
            let mut ok = true;
            for (i, item) in arr.iter().enumerate() {
                match item.as_f64() {
                    Some(v) => r[i] = v,
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return r;
            }
        }
    }
    // Fallback: union bbox of all blocks.
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for b in blocks {
        x0 = x0.min(b.bbox[0]);
        y0 = y0.min(b.bbox[1]);
        x1 = x1.max(b.bbox[2]);
        y1 = y1.max(b.bbox[3]);
    }
    if x0 == f64::MAX {
        [0.0, 0.0, 612.0, 792.0]
    } else {
        [x0, y0, x1, y1]
    }
}

fn extract_single_page(
    doc: &Document,
    page_ref: crate::types::ObjectId,
    mmap_data: &[u8],
    options: &ExtractOptions,
    page_idx: u32,
) -> ParseResult<PageResult> {
    let page = doc.get_object(page_ref.num)?;

    // Get content stream(s)
    let content_data = get_page_contents(doc, &page)?;

    // Resolve /Resources (may be indirect reference)
    let resources = match page.get(b"Resources") {
        Some(PdfObject::Ref(r)) => doc.get_object(r.num).ok(),
        other => other.cloned(),
    };

    // Build font map from /Resources
    let font_map = if let Some(ref resources) = resources {
        crate::font::build_font_map(resources, |obj_num| doc.get_object(obj_num))
    } else {
        HashMap::new()
    };

    // Build XObject map for Form recursion
    let xobjects = if let Some(ref resources) = resources {
        build_xobject_map(doc, resources, mmap_data)
    } else {
        HashMap::new()
    };

    // Get primary font info for clustering. The font_size is a page-level
    // scalar used as the threshold basis for layout heuristics (span gap,
    // line gap, block gap). The historical 12pt fallback works well in
    // practice because it sits above typical body text (9-10pt) and below
    // title text (12-14pt), giving balanced thresholds. Per-char sizes
    // remain available on CharInfo.size for future per-char clustering.
    let (font_name, font_flags) = font_map
        .iter()
        .next()
        .map(|(name, info)| (name.clone(), info.flags))
        .unwrap_or_else(|| ("Helvetica".to_string(), 0));

    // Scan content stream with font-aware decoding + Form XObject recursion
    let scan_result = crate::parser::content_stream::scan_content_stream_full(
        &content_data,
        &font_map,
        &xobjects,
        0,
    );

    let font_size = 12.0;

    // Split chars by rotation: body chars go through the normal cluster →
    // XY-cut pipeline; rotated chars (arXiv watermarks, vertical axis
    // labels) are clustered separately and appended at the end so they
    // don't perturb the body's reading order. Default behavior (no
    // include_rotated) drops rotated chars entirely.
    let (body_chars, rot_chars): (Vec<_>, Vec<_>) = if options.include_rotated {
        scan_result.chars.iter().cloned().partition(|c| !c.rotated)
    } else {
        (
            scan_result
                .chars
                .iter()
                .filter(|c| !c.rotated)
                .cloned()
                .collect(),
            Vec::new(),
        )
    };

    // Cluster body chars and sort into visual reading order (recursive XY-cut).
    // The diagnostics variant returns the count of blocks dropped by the
    // out-of-page margin filter.
    let blocks = cluster_chars(&body_chars, &font_name, font_size, 0, font_flags);
    let rect = page_rect(&page, &blocks);
    let (mut blocks, out_of_page_dropped) =
        crate::layout::reading_order_sort_with_diagnostics(blocks, rect);

    // Cluster rotated chars separately. Each connected run (sidebar, axis
    // label) becomes its own block. Use the transpose-then-cluster path so
    // 90°/270°-rotated text groups into proper lines instead of one char
    // per span. Append at end so body reading order is preserved — this
    // matches the documented behavior that rotated text is not woven into
    // the XY-cut output.
    if !rot_chars.is_empty() {
        let mut rot_blocks =
            crate::layout::cluster_rotated_chars(&rot_chars, &font_name, font_size, 0, font_flags);
        blocks.append(&mut rot_blocks);
    }

    // Assemble diagnostics. Detection runs regardless of include_rotated —
    // the user sees "N rotated chars were dropped" even in default mode,
    // and can re-extract with include_rotated=True to recover them.
    let rotated_char_count = scan_result.chars.iter().filter(|c| c.rotated).count();
    let diagnostics = PageDiagnostics {
        rotated_char_count,
        type3_char_count: scan_result.type3_char_count,
        undecoded_byte_count: scan_result.undecoded_byte_count,
        out_of_page_block_count: out_of_page_dropped,
        inline_image_count: scan_result.inline_image_count,
    };

    // Detect scanned page (heuristic: little text + large image covering the page)
    let is_scanned = detect_scanned(&blocks, &scan_result.images, &rect);

    // Extract hyperlinks on this page. Errors here are non-fatal — a broken
    // /Annots array shouldn't sink the whole page's text extraction.
    let links = crate::links::extract_page_links(doc, &page, page_idx).unwrap_or_default();

    // Resolve images if requested
    let images = if options.include_images && !scan_result.images.is_empty() {
        if let Some(ref resources) = resources {
            resolve_images(&scan_result.images, resources, mmap_data, |obj_num| {
                doc.get_object(obj_num)
            })
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    Ok(PageResult {
        blocks,
        images,
        is_scanned,
        rect,
        diagnostics,
        links,
    })
}

/// Get the concatenated content stream data for a page.
fn get_page_contents(doc: &Document, page: &PdfObject<'_>) -> ParseResult<Vec<u8>> {
    match page.get(b"Contents") {
        Some(PdfObject::Ref(r)) => {
            let obj = doc.get_object(r.num)?;
            get_stream_data(&obj)
        }
        Some(PdfObject::Array(arr)) => {
            let mut combined = Vec::new();
            for item in arr {
                if let Some(r) = item.as_ref() {
                    let obj = doc.get_object(r.num)?;
                    let data = get_stream_data(&obj)?;
                    combined.extend_from_slice(&data);
                    combined.push(b'\n');
                }
            }
            Ok(combined)
        }
        _ => Ok(Vec::new()),
    }
}

/// Get stream data, decompressing based on /Filter if present.
fn get_stream_data(obj: &PdfObject<'_>) -> ParseResult<Vec<u8>> {
    match obj {
        PdfObject::Stream { dict, data } => {
            let filter = dict.iter().find(|(k, _)| *k == b"Filter").map(|(_, v)| v);
            match filter {
                Some(f) => crate::parser::xref::decompress_stream(data, f),
                None => Ok(data.to_vec()),
            }
        }
        _ => Ok(Vec::new()),
    }
}

/// Build XObject map from /Resources for Form XObject recursion.
fn build_xobject_map<'a>(
    doc: &Document,
    resources: &PdfObject<'a>,
    _mmap_data: &'a [u8],
) -> HashMap<String, XObjectData> {
    let mut result = HashMap::new();

    let xobjects_dict = match resources.get(b"XObject") {
        Some(PdfObject::Dict(d)) => d,
        _ => return result,
    };

    for (name, xobj_ref) in xobjects_dict {
        let obj_name = String::from_utf8_lossy(name).to_string();

        let xobj = match xobj_ref {
            PdfObject::Ref(r) => match doc.get_object(r.num) {
                Ok(obj) => obj,
                _ => continue,
            },
            _ => continue,
        };

        let subtype = xobj
            .get(b"Subtype")
            .and_then(|v| v.as_name())
            .unwrap_or(b"");

        if subtype == b"Form" {
            // Extract Form XObject content stream
            if let PdfObject::Stream { dict, data } = &xobj {
                let filter = dict.iter().find(|(k, _)| *k == b"Filter").map(|(_, v)| v);
                let stream_data = match filter {
                    Some(f) => crate::parser::xref::decompress_stream(data, f)
                        .unwrap_or_else(|_| data.to_vec()),
                    None => data.to_vec(),
                };

                // Extract /Matrix (default identity)
                let matrix = xobj
                    .get(b"Matrix")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        let mut m = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
                        for (i, item) in arr.iter().enumerate().take(6) {
                            m[i] =
                                item.as_f64()
                                    .unwrap_or(if i == 0 || i == 3 { 1.0 } else { 0.0 });
                        }
                        m
                    })
                    .unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);

                // Extract /BBox
                let bbox = xobj
                    .get(b"BBox")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        let mut b = [0.0, 0.0, 0.0, 0.0];
                        for (i, item) in arr.iter().enumerate().take(4) {
                            b[i] = item.as_f64().unwrap_or(0.0);
                        }
                        b
                    })
                    .unwrap_or([0.0, 0.0, 0.0, 0.0]);

                // Extract fonts from Form XObject's own /Resources
                let form_fonts = if let Some(form_resources) = xobj.get(b"Resources") {
                    crate::font::build_font_map(form_resources, |obj_num| doc.get_object(obj_num))
                } else {
                    HashMap::new()
                };

                result.insert(
                    obj_name,
                    XObjectData::Form {
                        data: stream_data,
                        matrix,
                        bbox,
                        fonts: form_fonts,
                    },
                );
            }
        } else {
            result.insert(obj_name, XObjectData::Image);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::content_stream::{ImageRef, TextBlock, TextLine, TextSpan};

    fn make_block(text: &str) -> TextBlock {
        TextBlock {
            bbox: [0.0, 0.0, 100.0, 20.0],
            lines: vec![TextLine {
                bbox: [0.0, 0.0, 100.0, 20.0],
                spans: vec![TextSpan {
                    text: text.to_string(),
                    font: "Helvetica".to_string(),
                    size: 12.0,
                    color: 0,
                    bbox: [0.0, 0.0, 100.0, 20.0],
                    chars: vec![],
                    flags: 0,
                }],
            }],
        }
    }

    fn make_full_page_image() -> ImageRef {
        // Covers most of a 612x792 letter page
        ImageRef {
            name: "Im0".to_string(),
            bbox: [0.0, 0.0, 612.0, 792.0],
            obj_ref: None,
        }
    }

    fn letter_rect() -> [f64; 4] {
        [0.0, 0.0, 612.0, 792.0]
    }

    #[test]
    fn test_detect_scanned_full_page_image_no_text() {
        // No text blocks + one full-page image => scanned
        assert!(detect_scanned(
            &[],
            &[make_full_page_image()],
            &letter_rect()
        ));
    }

    #[test]
    fn test_detect_scanned_real_text_page() {
        // Plenty of text + no image => not scanned
        let blocks = vec![make_block(
            "This is a real text page with enough characters to pass the heuristic threshold.",
        )];
        assert!(!detect_scanned(&blocks, &[], &letter_rect()));
    }

    #[test]
    fn test_detect_scanned_below_text_threshold_with_image() {
        // Tiny amount of text (under 50 chars) + full-page image => scanned
        let blocks = vec![make_block("hi")];
        assert!(detect_scanned(
            &blocks,
            &[make_full_page_image()],
            &letter_rect()
        ));
    }

    #[test]
    fn test_detect_scanned_small_image_not_scanned() {
        // No text + small image (logo, not page-filling) => not scanned
        let small_image = ImageRef {
            name: "logo".to_string(),
            bbox: [0.0, 0.0, 50.0, 50.0],
            obj_ref: None,
        };
        assert!(!detect_scanned(&[], &[small_image], &letter_rect()));
    }

    #[test]
    fn test_detect_scanned_empty_page() {
        // No text, no images => not scanned (blank page, not scanned)
        assert!(!detect_scanned(&[], &[], &letter_rect()));
    }

    #[test]
    fn test_page_rect_prefers_mediabox() {
        // Construct a synthetic page object with a /MediaBox and verify
        // page_rect surfaces it.
        let page = PdfObject::Dict(vec![
            (
                &b"MediaBox"[..],
                PdfObject::Array(vec![
                    PdfObject::Integer(0),
                    PdfObject::Integer(0),
                    PdfObject::Real(595.0),
                    PdfObject::Real(842.0),
                ]),
            ),
            (&b"Type"[..], PdfObject::Name(b"Page")),
        ]);
        let rect = page_rect(&page, &[]);
        assert!((rect[0] - 0.0).abs() < 1e-6);
        assert!((rect[1] - 0.0).abs() < 1e-6);
        assert!((rect[2] - 595.0).abs() < 1e-6);
        assert!((rect[3] - 842.0).abs() < 1e-6);
    }

    #[test]
    fn test_page_rect_falls_back_to_blocks_bbox() {
        // No MediaBox: fallback is union bbox of all blocks.
        let blocks = vec![
            TextBlock {
                bbox: [10.0, 20.0, 100.0, 50.0],
                lines: vec![],
            },
            TextBlock {
                bbox: [5.0, 60.0, 80.0, 90.0],
                lines: vec![],
            },
        ];
        let empty_page: PdfObject<'_> = PdfObject::Dict(vec![]);
        let rect = page_rect(&empty_page, &blocks);
        // Union: x0=5, y0=20, x1=100, y1=90
        assert!((rect[0] - 5.0).abs() < 1e-6);
        assert!((rect[1] - 20.0).abs() < 1e-6);
        assert!((rect[2] - 100.0).abs() < 1e-6);
        assert!((rect[3] - 90.0).abs() < 1e-6);
    }
}
