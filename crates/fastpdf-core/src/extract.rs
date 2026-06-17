/// High-level extraction API: ties together document parsing, content stream
/// scanning, layout clustering, and image extraction into a single call.
use crate::document::Document;
use crate::image::{resolve_images, ExtractedImage};
use crate::layout::cluster_chars;
use crate::parser::content_stream::XObjectData;
use crate::parser::ParseResult;
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
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            page_parallel: true,
            file_parallel: true,
            include_images: true,
            gpu: false,
            batch_size: 50,
        }
    }
}

/// Result of extracting a single page.
#[derive(Debug, Clone)]
pub struct PageResult {
    pub blocks: Vec<crate::parser::TextBlock>,
    pub images: Vec<ExtractedImage>,
}

/// Result of extracting a document.
#[derive(Debug)]
pub struct ExtractResult {
    pub pages: Vec<PageResult>,
}

/// Extract text blocks and images from a single PDF file.
pub fn extract(path: &str, options: &ExtractOptions) -> ParseResult<ExtractResult> {
    let doc = Document::open(path)?;
    extract_doc(&doc, options)
}

/// Extract from an already-opened Document.
pub fn extract_doc(doc: &Document, options: &ExtractOptions) -> ParseResult<ExtractResult> {
    let page_refs = doc.page_refs()?;
    let mmap_data: &[u8] = unsafe { std::mem::transmute(doc.mmap_slice()) };

    // Auto-batch: if page count > batch_size and batch_size > 0, process in batches
    let batch_size = if options.batch_size > 0 && page_refs.len() > options.batch_size {
        options.batch_size
    } else {
        page_refs.len()
    };

    let mut all_pages = Vec::with_capacity(page_refs.len());

    for batch in page_refs.chunks(batch_size) {
        let batch_results = extract_page_batch(doc, batch, mmap_data, options);
        all_pages.extend(batch_results);
    }

    Ok(ExtractResult { pages: all_pages })
}

/// Extract a batch of pages (used for large document batching).
fn extract_page_batch(
    doc: &Document,
    page_refs: &[crate::types::ObjectId],
    mmap_data: &[u8],
    options: &ExtractOptions,
) -> Vec<PageResult> {
    if options.page_parallel && page_refs.len() > 1 {
        page_refs
            .par_iter()
            .map(|r| {
                extract_single_page(doc, *r, mmap_data, options).unwrap_or_else(|_| PageResult {
                    blocks: vec![],
                    images: vec![],
                })
            })
            .collect()
    } else {
        page_refs
            .iter()
            .map(|r| {
                extract_single_page(doc, *r, mmap_data, options).unwrap_or_else(|_| PageResult {
                    blocks: vec![],
                    images: vec![],
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

fn extract_single_page(
    doc: &Document,
    page_ref: crate::types::ObjectId,
    mmap_data: &[u8],
    options: &ExtractOptions,
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

    // Get primary font info for clustering
    let (font_name, font_size) = font_map
        .iter()
        .next()
        .map(|(name, _info)| (name.clone(), 12.0))
        .unwrap_or_else(|| ("Helvetica".to_string(), 12.0));

    // Scan content stream with font-aware decoding + Form XObject recursion
    let scan_result = crate::parser::content_stream::scan_content_stream_full(
        &content_data,
        &font_map,
        &xobjects,
        0,
    );

    // Cluster chars into blocks
    let blocks = cluster_chars(&scan_result.chars, &font_name, font_size, 0);

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

    Ok(PageResult { blocks, images })
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
