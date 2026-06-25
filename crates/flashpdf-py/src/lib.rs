use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
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
        let dict = PyDict::new(py);
        dict.set_item("uri", &link.uri)?;
        dict.set_item("bbox", link.bbox.to_vec())?;
        dict.set_item("page", link.page)?;
        output.append(dict)?;
    }

    Ok(output)
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

    /// No-op: the underlying mmap has already been released by the time
    /// `open()` returns. Provided for fitz API parity (`doc.close()`).
    fn close(&self) {}

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
        Ok(Self { pages })
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
                    // fitz flag bits (italic/bold/serif/...). Stub: 0 for now.
                    span_dict.set_item("flags", 0)?;
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
