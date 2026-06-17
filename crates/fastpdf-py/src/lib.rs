use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};

/// FastPDF — high-performance PDF text and image extraction.
#[pymodule]
fn fastpdf(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(extract, m)?)?;
    m.add_function(wrap_pyfunction!(extract_many, m)?)?;
    m.add_function(wrap_pyfunction!(extract_links, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

/// Extract text blocks and images from a single PDF file.
#[pyfunction]
#[pyo3(signature = (path, page_parallel=true, include_images=true, gpu=false, batch_size=50))]
fn extract<'py>(
    py: Python<'py>,
    path: &str,
    page_parallel: bool,
    include_images: bool,
    gpu: bool,
    batch_size: usize,
) -> PyResult<Bound<'py, PyList>> {
    let options = fastpdf_core::ExtractOptions {
        page_parallel,
        file_parallel: false,
        include_images,
        gpu,
        batch_size,
    };

    let result = fastpdf_core::extract(path, &options)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    render_extract_result(py, &result)
}

/// Batch extract from multiple PDF files with file-level parallelism.
#[pyfunction]
#[pyo3(signature = (paths, file_parallel=true, page_parallel=false, include_images=false, gpu=false, batch_size=50))]
fn extract_many<'py>(
    py: Python<'py>,
    paths: Vec<String>,
    file_parallel: bool,
    page_parallel: bool,
    include_images: bool,
    gpu: bool,
    batch_size: usize,
) -> PyResult<Bound<'py, PyList>> {
    let options = fastpdf_core::ExtractOptions {
        page_parallel,
        file_parallel,
        include_images,
        gpu,
        batch_size,
    };

    let path_refs: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    let results = fastpdf_core::extract_many(&path_refs, &options);

    let output = PyList::empty(py);
    for (path, result) in results {
        let item = PyList::empty(py);
        item.append(path)?;

        match result {
            Ok(extract_result) => {
                let page_result = render_extract_result(py, &extract_result)?;
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
    let doc = fastpdf_core::Document::open(path)
        .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

    let links = fastpdf_core::extract_links(&doc)
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

/// Render an ExtractResult into a Python list of [blocks, images].
fn render_extract_result<'py>(
    py: Python<'py>,
    result: &fastpdf_core::ExtractResult,
) -> PyResult<Bound<'py, PyList>> {
    let blocks_list = PyList::empty(py);
    let images_list = PyList::empty(py);

    for page in &result.pages {
        for block in &page.blocks {
            let block_dict = PyDict::new(py);
            block_dict.set_item("type", 0)?;
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
                Some(fastpdf_core::ImageData::Raw(data)) => {
                    img_dict.set_item("image", PyBytes::new(py, data))?;
                }
                Some(fastpdf_core::ImageData::Png(data)) => {
                    img_dict.set_item("image", PyBytes::new(py, data))?;
                }
                _ => {
                    img_dict.set_item("image", py.None())?;
                }
            }
            images_list.append(img_dict)?;
        }
    }

    let result_list = PyList::empty(py);
    result_list.append(blocks_list)?;
    result_list.append(images_list)?;
    Ok(result_list)
}
