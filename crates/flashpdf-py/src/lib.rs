use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList, PyString};
use std::sync::Arc;

/// FlashPDF — high-performance PDF text and image extraction.
#[pymodule]
fn flashpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(extract_many, m)?)?;
    m.add_function(wrap_pyfunction!(extract_links, m)?)?;
    m.add_function(wrap_pyfunction!(open, m)?)?;
    m.add_class::<PyDocument>()?;
    m.add_class::<PyPage>()?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// fitz-style entry point: open a PDF and eagerly extract all pages.
///
/// Returns a `Document` whose pages can be indexed (`doc[i]`) and queried
/// via `page.get_text("dict"|"text"|"blocks")`, `page.get_images()`,
/// `page.is_scanned`, `page.rect`. Mirrors `fitz.open(path)` for the
/// text/image-extraction subset of the API.
///
/// `include_images=False` skips image extraction (faster when you only need
/// text). Pages are shared with `Arc` internally, so `doc[i]` is O(1) —
/// no per-access clone of the page's blocks/images.
#[pyfunction]
#[pyo3(signature = (path, include_images=true, include_rotated=false))]
fn open(path: &str, include_images: bool, include_rotated: bool) -> PyResult<PyDocument> {
    PyDocument::open(path, include_images, include_rotated)
}

/// Extract text blocks and images from a single PDF file.
///
/// Returns `(blocks, images)` by default. Pass `with_page_info=True` to get
/// a third element `pages` — a list of `{page, is_scanned}` dicts, useful for
/// detecting scanned pages that need OCR.
#[pyfunction]
#[pyo3(signature = (path, page_parallel=true, include_images=true, gpu=false, batch_size=50, with_page_info=false, include_rotated=false))]
#[allow(clippy::too_many_arguments)]
fn extract<'py>(
    py: Python<'py>,
    path: &str,
    page_parallel: bool,
    include_images: bool,
    gpu: bool,
    batch_size: usize,
    with_page_info: bool,
    include_rotated: bool,
) -> PyResult<Bound<'py, PyList>> {
    let options = flashpdf_core::ExtractOptions {
        page_parallel,
        file_parallel: false,
        include_images,
        gpu,
        batch_size,
        include_rotated,
    };

    let result = flashpdf_core::extract(path, &options)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    render_extract_result(py, &result, with_page_info)
}

/// Batch extract from multiple PDF files with file-level parallelism.
#[pyfunction]
#[pyo3(signature = (paths, file_parallel=true, page_parallel=false, include_images=false, gpu=false, batch_size=50, with_page_info=false, include_rotated=false))]
#[allow(clippy::too_many_arguments)]
fn extract_many<'py>(
    py: Python<'py>,
    paths: Vec<String>,
    file_parallel: bool,
    page_parallel: bool,
    include_images: bool,
    gpu: bool,
    batch_size: usize,
    with_page_info: bool,
    include_rotated: bool,
) -> PyResult<Bound<'py, PyList>> {
    let options = flashpdf_core::ExtractOptions {
        page_parallel,
        file_parallel,
        include_images,
        gpu,
        batch_size,
        include_rotated,
    };

    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let results = flashpdf_core::extract_many(&path_refs, &options);

    let output = PyList::empty(py);
    for (path, result) in results {
        let item = PyList::empty(py);
        item.append(path)?;

        match result {
            Ok(extract_result) => {
                let page_result = render_extract_result(py, &extract_result, with_page_info)?;
                item.append(page_result)?;
            }
            Err(_) => {
                item.append(PyList::empty(py))?;
            }
        }
        output.append(item)?;
    }

    Ok(output)
}

/// Extract hyperlinks from a PDF file.
#[pyfunction]
fn extract_links<'py>(py: Python<'py>, path: &str) -> PyResult<Bound<'py, PyList>> {
    let doc = flashpdf_core::Document::open(path)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    let links = flashpdf_core::extract_links(&doc)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    let output = PyList::empty(py);
    for link in &links {
        output.append(link_to_dict(py, link)?)?;
    }

    Ok(output)
}

/// Map a Rust `PageLink` to a fitz-style Python dict. Used by both
/// `extract_links` (the standalone function) and `Page.get_links()`.
fn link_to_dict<'py>(
    py: Python<'py>,
    link: &flashpdf_core::PageLink,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    let kind_str = match link.kind {
        flashpdf_core::LinkKind::Uri => "uri",
        flashpdf_core::LinkKind::Goto => "goto",
        flashpdf_core::LinkKind::Named => "named",
        flashpdf_core::LinkKind::Launch => "launch",
        flashpdf_core::LinkKind::GotoR => "gotor",
    };
    d.set_item("kind", kind_str)?;
    // fitz uses `from` for the link's bbox.
    d.set_item("from", link.bbox.to_vec())?;
    // Also expose as `bbox` for users who don't recognize the fitz name.
    d.set_item("bbox", link.bbox.to_vec())?;
    d.set_item("page", link.page)?;

    // Kind-specific fields. fitz puts these on the same dict, with `None`
    // for inapplicable ones — we mirror that so callers can write
    // `link["uri"]` without checking the kind first.
    d.set_item("uri", opt_str_to_py(py, &link.uri))?;
    d.set_item(
        "to",
        match link.to_page {
            Some(p) => p.into_pyobject(py)?.into_any(),
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    d.set_item(
        "to_point",
        match link.to_point {
            Some([x, y]) => {
                let arr = PyList::empty(py);
                arr.append(x)?;
                arr.append(y)?;
                arr.into_any()
            }
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    d.set_item("name", opt_str_to_py(py, &link.name))?;
    d.set_item("file", opt_str_to_py(py, &link.file))?;
    d.set_item(
        "remote_page",
        match link.remote_page {
            Some(p) => p.into_pyobject(py)?.into_any(),
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    Ok(d)
}

/// Map `Option<&str>` (treating empty as None) to a Python str or None.
fn opt_str_to_py<'py>(py: Python<'py>, s: &str) -> Bound<'py, PyAny> {
    if s.is_empty() {
        py.None().into_bound(py)
    } else {
        pyo3::types::PyString::new(py, s).into_any()
    }
}

/// Render a TocItem as a fitz-style dict (rich mode of `get_toc`).
fn toc_item_to_dict<'py>(
    py: Python<'py>,
    item: &flashpdf_core::TocItem,
) -> PyResult<Bound<'py, PyDict>> {
    let d = PyDict::new(py);
    d.set_item("level", item.level)?;
    d.set_item("title", &item.title)?;
    // 1-based page (0 = unresolved) to match fitz simple mode; the raw 0-based
    // value is exposed as `page0` for callers that want to index `doc[i]`.
    let page_1b = item.page.map(|p| p as i64 + 1).unwrap_or(0);
    d.set_item("page", page_1b)?;
    d.set_item("page0", {
        match item.page {
            Some(p) => p.into_pyobject(py)?.into_any(),
            None => py.None().into_bound(py).into_any(),
        }
    })?;
    d.set_item(
        "kind",
        match item.kind {
            Some(flashpdf_core::LinkKind::Uri) => "uri",
            Some(flashpdf_core::LinkKind::Goto) => "goto",
            Some(flashpdf_core::LinkKind::Named) => "named",
            Some(flashpdf_core::LinkKind::Launch) => "launch",
            Some(flashpdf_core::LinkKind::GotoR) => "gotor",
            None => "",
        },
    )?;
    d.set_item(
        "uri",
        match &item.uri {
            Some(s) => PyString::new(py, s).into_any(),
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    d.set_item(
        "to_point",
        match item.to_point {
            Some([x, y]) => {
                let arr = PyList::empty(py);
                arr.append(x)?;
                arr.append(y)?;
                arr.into_any()
            }
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    d.set_item(
        "name",
        match &item.name {
            Some(s) => PyString::new(py, s).into_any(),
            None => py.None().into_bound(py).into_any(),
        },
    )?;
    Ok(d)
}

/// Render an ExtractResult into a Python list of [blocks, images] or
/// [blocks, images, pages] when `with_page_info` is set.
fn render_extract_result<'py>(
    py: Python<'py>,
    result: &flashpdf_core::ExtractResult,
    with_page_info: bool,
) -> PyResult<Bound<'py, PyList>> {
    let blocks_list = PyList::empty(py);
    let images_list = PyList::empty(py);
    let pages_list = PyList::empty(py);

    for (page_idx, page) in result.pages.iter().enumerate() {
        for block in &page.blocks {
            let block_dict = PyDict::new(py);
            block_dict.set_item("type", 0)?;
            block_dict.set_item("page", page_idx)?;
            block_dict.set_item("bbox", block.bbox.to_vec())?;

            let lines_list = PyList::empty(py);
            for line in &block.lines {
                let line_dict = PyDict::new(py);
                line_dict.set_item("bbox", line.bbox.to_vec())?;

                let spans_list = PyList::empty(py);
                for span in &line.spans {
                    let span_dict = PyDict::new(py);
                    span_dict.set_item("bbox", span.bbox.to_vec())?;
                    span_dict.set_item("text", &span.text)?;
                    span_dict.set_item("font", &span.font)?;
                    span_dict.set_item("size", span.size)?;
                    span_dict.set_item("color", span.color)?;
                    span_dict.set_item("flags", span.flags)?;
                    spans_list.append(span_dict)?;
                }
                line_dict.set_item("spans", spans_list)?;
                lines_list.append(line_dict)?;
            }
            block_dict.set_item("lines", lines_list)?;
            blocks_list.append(block_dict)?;
        }

        for img in &page.images {
            let img_dict = PyDict::new(py);
            img_dict.set_item("bbox", img.bbox.to_vec())?;
            img_dict.set_item("width", img.width)?;
            img_dict.set_item("height", img.height)?;
            img_dict.set_item("bpc", img.bpc)?;
            img_dict.set_item("colorspace", &img.colorspace)?;
            img_dict.set_item("xref", img.xref)?;
            img_dict.set_item("ext", &img.ext)?;

            match &img.data {
                Some(flashpdf_core::ImageData::Raw(data)) => {
                    img_dict.set_item("image", PyBytes::new(py, data))?;
                }
                Some(flashpdf_core::ImageData::Png(data)) => {
                    img_dict.set_item("image", PyBytes::new(py, data))?;
                }
                _ => {
                    img_dict.set_item("image", py.None())?;
                }
            }
            images_list.append(img_dict)?;
        }

        if with_page_info {
            let page_dict = PyDict::new(py);
            page_dict.set_item("page", page_idx)?;
            page_dict.set_item("is_scanned", page.is_scanned)?;
            let diag = &page.diagnostics;
            let diag_dict = PyDict::new(py);
            diag_dict.set_item("rotated_char_count", diag.rotated_char_count)?;
            diag_dict.set_item("type3_char_count", diag.type3_char_count)?;
            diag_dict.set_item("undecoded_byte_count", diag.undecoded_byte_count)?;
            diag_dict.set_item("out_of_page_block_count", diag.out_of_page_block_count)?;
            page_dict.set_item("diagnostics", diag_dict)?;
            pages_list.append(page_dict)?;
        }
    }

    let result_list = PyList::empty(py);
    result_list.append(blocks_list)?;
    result_list.append(images_list)?;
    if with_page_info {
        result_list.append(pages_list)?;
    }
    Ok(result_list)
}

// ============================================================================
// fitz-style API: Document / Page
// ============================================================================

/// A parsed PDF document with all pages eagerly extracted on open.
///
/// Mirrors `fitz.Document` for the text/image extraction subset of the API.
/// Use `len(doc)` for page count, `doc[i]` to get a `Page`, and
/// `page.get_text("dict")` for fitz-compatible dict output.
#[pyclass(name = "Document")]
pub struct PyDocument {
    pages: Vec<Arc<flashpdf_core::PageResult>>,
    metadata: flashpdf_core::DocumentMetadata,
    /// PDF version string from `%PDF-X.Y` header (e.g. `"1.7"`), or `None`
    /// when the header is missing/malformed. Surfaced as part of
    /// `doc.metadata["format"]` for fitz parity (`"PDF 1.7"`).
    pdf_version: Option<String>,
    /// Outline / table of contents, extracted by walking `/Outlines` at
    /// open time. Empty when the PDF has no outline.
    toc: Vec<flashpdf_core::TocItem>,
}

#[pymethods]
impl PyDocument {
    #[new]
    #[pyo3(signature = (path, include_images=true, include_rotated=false))]
    fn new(path: &str, include_images: bool, include_rotated: bool) -> PyResult<Self> {
        Self::open(path, include_images, include_rotated)
    }

    /// Number of pages. `len(doc)`.
    fn __len__(&self) -> usize {
        self.pages.len()
    }

    /// Index a page. Supports negative indices (`doc[-1]`).
    ///
    /// O(1): bumps an `Arc` refcount, does NOT clone the page's blocks/images.
    fn __getitem__(&self, py: Python<'_>, idx: isize) -> PyResult<Py<PyPage>> {
        let i = if idx < 0 {
            self.pages.len() as isize + idx
        } else {
            idx
        };
        if i < 0 || i as usize >= self.pages.len() {
            return Err(pyo3::exceptions::PyIndexError::new_err(format!(
                "page index {idx} out of range 0..{}",
                self.pages.len()
            )));
        }
        // Arc::clone is a single atomic refcount bump — no deep copy of
        // blocks/images vectors. PyPage shares ownership of the PageResult.
        let page = PyPage {
            page_idx: i as usize,
            page: Arc::clone(&self.pages[i as usize]),
        };
        Py::new(py, page)
    }

    /// Page count (fitz-compatible property name).
    #[getter]
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Document metadata (fitz-compatible dict). Always returns the same
    /// key set as PyMuPDF: `title`, `author`, `subject`, `keywords`,
    /// `creator`, `producer`, `creationDate`, `modDate`, `format`,
    /// `encryption`, `size`. Missing fields are `None`.
    #[getter]
    fn metadata<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        let m = &self.metadata;
        d.set_item("title", opt_to_py(py, &m.title))?;
        d.set_item("author", opt_to_py(py, &m.author))?;
        d.set_item("subject", opt_to_py(py, &m.subject))?;
        d.set_item("keywords", opt_to_py(py, &m.keywords))?;
        d.set_item("creator", opt_to_py(py, &m.creator))?;
        d.set_item("producer", opt_to_py(py, &m.producer))?;
        d.set_item("creationDate", opt_to_py(py, &m.creation_date))?;
        d.set_item("modDate", opt_to_py(py, &m.mod_date))?;
        // fitz reports the file format and encryption state — we expose the
        // same keys for API parity. flashpdf is read-only so these are static.
        let format = match &self.pdf_version {
            Some(v) => format!("PDF {}", v),
            None => "PDF".to_string(),
        };
        d.set_item("format", format)?;
        d.set_item("encryption", py.None())?;
        d.set_item("size", py.None())?;
        Ok(d)
    }

    /// No-op: the underlying mmap has already been released by the time
    /// `open()` returns. Provided for fitz API parity (`doc.close()`).
    fn close(&self) {}

    /// Document outline (table of contents). Mirrors `fitz.Document.get_toc`.
    ///
    /// - `simple=True` (default): list of `[level, title, page]` where
    ///   `page` is 1-based (0 means unresolved), matching fitz simple mode.
    /// - `simple=False`: list of dicts with extra fields (`kind`, `uri`,
    ///   `to_point`, `name`) for richer link-type detection.
    #[pyo3(signature = (simple=true))]
    fn get_toc<'py>(&self, py: Python<'py>, simple: bool) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for item in &self.toc {
            if simple {
                // fitz convention: 1-based page (0 = unresolved)
                let page_1b = item.page.map(|p| p as i64 + 1).unwrap_or(0);
                let row = PyList::empty(py);
                row.append(item.level as i64)?;
                row.append(&item.title)?;
                row.append(page_1b)?;
                list.append(row)?;
            } else {
                list.append(toc_item_to_dict(py, item)?)?;
            }
        }
        Ok(list)
    }

    fn __enter__(slf: Bound<'_, Self>) -> Bound<'_, Self> {
        slf
    }

    fn __exit__<'py>(
        &self,
        _exc_type: &Bound<'py, PyAny>,
        _exc_val: &Bound<'py, PyAny>,
        _tb: &Bound<'py, PyAny>,
    ) -> PyResult<()> {
        Ok(())
    }
}

impl PyDocument {
    fn open(path: &str, include_images: bool, include_rotated: bool) -> PyResult<Self> {
        let opts = flashpdf_core::ExtractOptions {
            page_parallel: true,
            file_parallel: false,
            include_images,
            gpu: false,
            batch_size: 50,
            include_rotated,
        };
        let result = flashpdf_core::extract(path, &opts)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let pages = result.pages.into_iter().map(Arc::new).collect();

        // Outline extraction: re-open as a Document to walk /Outlines. The
        // mmap is cheap; the alternative would be threading toc through
        // ExtractResult, but outline extraction is independent of the page
        // extraction pipeline and easier to keep isolated here.
        let toc = match flashpdf_core::Document::open(path) {
            Ok(doc) => flashpdf_core::extract_toc(&doc).unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        Ok(Self {
            pages,
            metadata: result.metadata,
            pdf_version: result.pdf_version,
            toc,
        })
    }
}

/// Map an `Option<String>` to `Option<&str>` → PyObject without consuming
/// the inner String, so `metadata` can be read repeatedly across `getters`.
fn opt_to_py<'py>(py: Python<'py>, s: &Option<String>) -> Bound<'py, PyAny> {
    match s {
        Some(v) => pyo3::types::PyString::new(py, v).into_any(),
        None => py.None().into_bound(py),
    }
}

/// A single PDF page view. Returned by `doc[i]`.
///
/// `get_text("dict")` returns a fitz-compatible dict with text blocks
/// (`type=0`) and image blocks (`type=1`) in one `blocks` list.
/// `get_text("text")` returns plain text. `get_text("blocks")` returns
/// the simplified `(bbox, text, block_no, block_type)` tuple list.
#[pyclass(name = "Page")]
pub struct PyPage {
    page_idx: usize,
    /// Arc-shared with the parent Document. Cloning a Page (via `doc[i]`)
    /// is a single atomic refcount bump — no deep copy of blocks/images.
    page: Arc<flashpdf_core::PageResult>,
}

#[pymethods]
impl PyPage {
    /// Extract text in fitz-compatible modes: "dict" (default), "text", "blocks".
    #[pyo3(signature = (mode="dict"))]
    fn get_text<'py>(&self, py: Python<'py>, mode: &str) -> PyResult<Bound<'py, PyAny>> {
        match mode {
            "dict" => self.text_dict(py).map(|b| b.into_any()),
            "text" => self.text_plain(py).map(|b| b.into_any()),
            "blocks" => self.text_blocks(py).map(|b| b.into_any()),
            other => Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown get_text mode: {other:?} (expected 'dict', 'text', or 'blocks')"
            ))),
        }
    }

    /// fitz-compatible image list. Each image is a dict with bbox/width/height/
    /// bpc/colorspace/xref/ext/image keys.
    fn get_images<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for img in &self.page.images {
            list.append(self.image_to_dict(py, img)?)?;
        }
        Ok(list)
    }

    /// fitz-compatible link list. Each link is a dict with `kind` (`"uri"` /
    /// `"goto"` / `"named"` / `"launch"` / `"gotor"`), `from` (bbox), `page`
    /// (the page hosting the link), and kind-specific fields: `uri`, `to`
    /// (target page index), `to_point`, `name`, `file`, `remote_page`.
    fn get_links<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for link in &self.page.links {
            list.append(link_to_dict(py, link)?)?;
        }
        Ok(list)
    }

    #[getter]
    fn is_scanned(&self) -> bool {
        self.page.is_scanned
    }

    /// fitz uses `number` for the 0-based page index.
    #[getter]
    fn number(&self) -> usize {
        self.page_idx
    }

    #[getter]
    fn rect(&self) -> [f64; 4] {
        self.page.rect
    }

    /// Alias for `rect`, for PyMuPDF-style access.
    #[getter]
    fn bbox(&self) -> [f64; 4] {
        self.page.rect
    }

    /// Per-page diagnostics: counts of content that was dropped or
    /// potentially mis-decoded. Non-zero values tell the user "flashpdf
    /// found N items it couldn't faithfully extract"; the user can then
    /// decide whether to re-extract with different flags (e.g.
    /// `include_rotated=True`) or feed the page to an OCR pipeline.
    ///
    /// Returns a dict with keys:
    ///   - `rotated_char_count`: chars under a rotated/sheared text matrix
    ///     (arXiv sidebars, vertical axis labels). Recoverable via
    ///     `open(..., include_rotated=True)`.
    ///   - `type3_char_count`: chars under a /Type3 font (glyph-as-content-stream).
    ///     Positioning may be off; glyphs without /ToUnicode are unreadable.
    ///   - `undecoded_byte_count`: bytes that mapped to U+FFFD (missing
    ///     /ToUnicode or /Encoding).
    ///   - `out_of_page_block_count`: blocks dropped by the reading-order
    ///     margin filter (bbox extends >10% outside the page rect).
    #[getter]
    fn diagnostics<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let d = PyDict::new(py);
        let diag = &self.page.diagnostics;
        d.set_item("rotated_char_count", diag.rotated_char_count)?;
        d.set_item("type3_char_count", diag.type3_char_count)?;
        d.set_item("undecoded_byte_count", diag.undecoded_byte_count)?;
        d.set_item("out_of_page_block_count", diag.out_of_page_block_count)?;
        Ok(d)
    }
}

impl PyPage {
    fn text_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let blocks_list = PyList::empty(py);

        // Text blocks (type=0)
        for block in &self.page.blocks {
            let block_dict = PyDict::new(py);
            block_dict.set_item("type", 0)?;
            block_dict.set_item("bbox", block.bbox.to_vec())?;
            block_dict.set_item("number", self.page_idx)?;

            let lines_list = PyList::empty(py);
            for line in &block.lines {
                let line_dict = PyDict::new(py);
                line_dict.set_item("bbox", line.bbox.to_vec())?;

                let spans_list = PyList::empty(py);
                for span in &line.spans {
                    let span_dict = PyDict::new(py);
                    span_dict.set_item("bbox", span.bbox.to_vec())?;
                    span_dict.set_item("text", &span.text)?;
                    span_dict.set_item("font", &span.font)?;
                    span_dict.set_item("size", span.size)?;
                    span_dict.set_item("color", span.color)?;
                    // fitz flag bits: italic=2, serif=4, mono=8, bold=16.
                    span_dict.set_item("flags", span.flags)?;
                    spans_list.append(span_dict)?;
                }
                line_dict.set_item("spans", spans_list)?;
                lines_list.append(line_dict)?;
            }
            block_dict.set_item("lines", lines_list)?;
            blocks_list.append(block_dict)?;
        }

        // Image blocks (type=1), inline like fitz
        for img in &self.page.images {
            let img_dict = self.image_to_dict(py, img)?;
            // fitz image blocks include type=1 at the same level as text blocks
            img_dict.set_item("type", 1)?;
            blocks_list.append(img_dict)?;
        }

        let dict = PyDict::new(py);
        dict.set_item("blocks", blocks_list)?;
        Ok(dict)
    }

    fn text_plain<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, pyo3::types::PyString>> {
        let mut out = String::new();
        for block in &self.page.blocks {
            for line in &block.lines {
                for span in &line.spans {
                    out.push_str(&span.text);
                }
                out.push('\n');
            }
            out.push('\n');
        }
        Ok(pyo3::types::PyString::new(py, &out))
    }

    fn text_blocks<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyList>> {
        // fitz "blocks" mode: list of (x0, y0, x1, y1, text, block_no, block_type)
        let list = PyList::empty(py);
        for (i, block) in self.page.blocks.iter().enumerate() {
            let text: String = block
                .lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.text.as_str()))
                .collect::<Vec<_>>()
                .join("");
            let tuple = (
                block.bbox[0],
                block.bbox[1],
                block.bbox[2],
                block.bbox[3],
                text,
                i,
                0_i32, // block_type: 0 = text
            );
            list.append(tuple)?;
        }
        for (i, img) in self.page.images.iter().enumerate() {
            let tuple = (
                img.bbox[0],
                img.bbox[1],
                img.bbox[2],
                img.bbox[3],
                "",
                self.page.blocks.len() + i,
                1_i32, // block_type: 1 = image
            );
            list.append(tuple)?;
        }
        Ok(list)
    }

    fn image_to_dict<'py>(
        &self,
        py: Python<'py>,
        img: &flashpdf_core::ExtractedImage,
    ) -> PyResult<Bound<'py, PyDict>> {
        let img_dict = PyDict::new(py);
        img_dict.set_item("bbox", img.bbox.to_vec())?;
        img_dict.set_item("width", img.width)?;
        img_dict.set_item("height", img.height)?;
        img_dict.set_item("bpc", img.bpc)?;
        img_dict.set_item("colorspace", &img.colorspace)?;
        img_dict.set_item("xref", img.xref)?;
        img_dict.set_item("ext", &img.ext)?;
        match &img.data {
            Some(flashpdf_core::ImageData::Raw(data)) => {
                img_dict.set_item("image", PyBytes::new(py, data))?;
            }
            Some(flashpdf_core::ImageData::Png(data)) => {
                img_dict.set_item("image", PyBytes::new(py, data))?;
            }
            // MmapSlice is unreachable in practice (resolve_images only
            // produces Raw). Match existing extract() behavior: emit None.
            _ => {
                img_dict.set_item("image", py.None())?;
            }
        }
        Ok(img_dict)
    }
}
