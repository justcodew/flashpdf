# flashpdf API reference

**English** · [简体中文](API.md)

## Python API

### `flashpdf.extract(path, page_parallel=True, include_images=True, gpu=False, batch_size=50)`

Extract text blocks and images from a single PDF file.

**Parameters:**

- `path` (str): path to the PDF file
- `page_parallel` (bool): enable page-level parallel processing. Multi-page PDFs get 2–4× speedup on multi-core CPUs. Default `True`
- `include_images` (bool): whether to extract raw image bytes. Disabling significantly reduces memory. Default `True`
- `gpu` (bool): enable GPU-accelerated image processing (requires NVIDIA GPU + CUDA). Default `False`
- `batch_size` (int): pages per batch for large-document processing. PDFs larger than this are processed in batches to control memory. Set to 0 to disable batching. Default `50`

**Returns:** `tuple(blocks, images)`

**Examples:**

```python
import flashpdf

# Basic usage
blocks, images = flashpdf.extract("report.pdf")

# Text only (faster, less memory)
blocks, _ = flashpdf.extract("report.pdf", include_images=False)

# Large-document tuning
blocks, images = flashpdf.extract("huge.pdf", batch_size=100)
```

---

### `flashpdf.extract_many(paths, file_parallel=True, page_parallel=False, include_images=False, gpu=False, batch_size=50)`

Batch-extract multiple PDF files. Supports file-level parallelism and async prefetch.

**Parameters:**

- `paths` (list[str]): list of PDF file paths
- `file_parallel` (bool): file-level parallel processing. Multiple files are parsed simultaneously. Default `True`
- `page_parallel` (bool): page-level parallelism. Enabling alongside `file_parallel` may over-subscribe; pick one. Default `False`
- `include_images` (bool): whether to extract images. Recommended off for batch workloads. Default `False`
- `gpu` (bool): GPU acceleration. Default `False`
- `batch_size` (int): batch size. Default `50`

**Returns:** `list[tuple(path, blocks, images)]`

**Example:**

```python
import flashpdf
import glob

paths = glob.glob("pdfs/*.pdf")

# File-level parallelism, text only
for path, blocks, images in flashpdf.extract_many(paths, include_images=False):
    text = " ".join(
        span["text"]
        for b in blocks
        for l in b["lines"]
        for span in l["spans"]
    )
    print(f"{path}: {len(text)} chars")
```

---

### `flashpdf.open(path, *, include_images=True, include_rotated=False, page_parallel=True, render_only=False) -> Document`

fitz-style entry. `open()` performs a one-shot parallel extraction of all pages (unless `render_only=True`);
subsequent `doc[i]` / `page.get_text()` calls are pure in-memory queries.

**Parameters:**

- `path` (str): path to the PDF file
- `include_images` (bool): whether to extract image bytes. Set `False` for text-only workloads to save decode time. Default `True`
- `include_rotated` (bool): whether to extract 90°/270° rotated characters (arXiv side-bar watermarks, chart vertical-axis labels). Default `False`
- `page_parallel` (bool): page-level parallelism (rayon). 2–4× speedup on multi-page single files. Default `True`
- `render_only` (bool): **new in v0.7.1**, skips eager text/image extraction for render-only workloads.
  When on, `get_text()` / `get_images()` return empty, `page.rect` / `is_scanned` are stubs,
  but `len(doc)` / `doc[i]` / `get_pixmap()` work normally. Default `False`

**Returns:** `Document` context manager (supports the `with` statement)

**Examples:**

```python
import flashpdf

# fitz-style: open + per-page queries
with flashpdf.open("paper.pdf") as doc:
    print(len(doc))                  # page count
    page = doc[0]                    # first page (supports doc[-1] negative indexing)
    d  = page.get_text("dict")       # structured dict
    t  = page.get_text("text")       # plain text
    bs = page.get_text("blocks")     # fitz "blocks" tuple list
    imgs = page.get_images()         # embedded images on this page
    print(page.is_scanned, page.rect, page.number)

# Render-only: skip text extraction to save time
with flashpdf.open("paper.pdf", render_only=True) as doc:
    png = doc[0].get_pixmap(dpi=150)
```

**Which API to use:**

| Scenario | Recommended |
|---|---|
| Interactive / per-page random access | `open()` |
| Batch vectorization (thousands of PDFs) | `extract_many(file_parallel=True)` |
| One-shot single file | `extract()` |
| Pure render thumbnails | `open(render_only=True)` + `get_pixmap()` |

---

### `Document`

Returned by `open()`; supports `len()` / indexing / iteration / context management.

| API | Description |
|---|---|
| `len(doc)` | Page count |
| `doc[i]` / `doc[-1]` | Get page (supports negative indexing), returns `Page` |
| `for page in doc` | Iterate all pages |
| `doc.metadata` | Metadata dict (`title` / `author` / `subject` / `creator` / `producer` / `creationDate` / `modDate`, etc.) |
| `doc.get_toc()` | Table of contents (outline / TOC), returns `[(level, title, page, kind, ...), ...]` |
| `with doc: ...` | Context management (resource cleanup) |

---

### `Page`

Obtained via `doc[i]`. Extraction completes at `open()` time, so all methods/attributes are pure in-memory queries.

| API | Description |
|---|---|
| `page.get_text(mode)` | `"dict"` (default) / `"text"` / `"blocks"`, aligned with fitz |
| `page.get_images()` | List of embedded images on the page (Do references + BI/ID/EI inline) |
| `page.get_links()` | List of hyperlinks on the page (v0.4.0+) |
| `page.get_pixmap(dpi=150, output=None)` | **new in v0.7.1**, render to PNG bytes. Requires the `render` feature + PDFium binary. When `output` is given, also writes the file. Without the feature it raises `NotImplementedError` |
| `page.is_scanned: bool` | Scanned-page heuristic (v0.1.4) |
| `page.rect: [x0, y0, x1, y1]` | MediaBox |
| `page.number: int` | 0-based page number |
| `page.diagnostics: dict` | See table below |

**`page.diagnostics` fields:**

| Field | Meaning | Suggested follow-up |
|---|---|---|
| `rotated_char_count` | Characters under a non-axis-aligned text matrix | Re-extract with `include_rotated=True` |
| `type3_char_count` | Characters under a Type3 font | Check whether a dedicated Type 3 handler or OCR is needed |
| `undecoded_byte_count` | Bytes that failed decoding and fell back to U+FFFD | Mostly font-subsetting artifacts; OCR can recover |
| `out_of_page_block_count` | Blocks dropped by the reading-order margin filter | Usually vector-art mis-clusters or rotated text out of bounds |
| `inline_image_count` | Inline images embedded by BI/ID/EI operators | Common in old scanned PDFs / receipts / Office exports |

---

### `page.get_pixmap(dpi=150, output=None) -> bytes`

**New in v0.7.1**. Calls PDFium to render the current page as PNG.

**Prerequisites:**

1. flashpdf built with `--features render` (default PyPI wheel does not include it; build from source — see [RENDERING.md](RENDERING.md))
2. A PDFium binary on the system path (`PDFIUM_PATH` env / `./pdfium-bin/` / system library)

**Parameters:**

- `dpi` (int): output resolution. 72 DPI = native PDF size, 150 DPI = screen preview, 300 DPI = print. Default `150`
- `output` (str | None): when given, PNG is also written to this path

**Returns:** PNG bytes (`bytes`)

**Not implemented (vs fitz):**

- ❌ `clip` / `matrix` / `colorspace` / `alpha` parameters
- ❌ PIL / numpy interop (user does `PIL.Image.open(io.BytesIO(png))`)
- ❌ raw RGBA output (always RGBA + white-background PNG)
- ❌ multiple output formats (PNG only)

**Examples:**

```python
import flashpdf

# Get PNG bytes directly
with flashpdf.open("paper.pdf", render_only=True) as doc:
    png = doc[0].get_pixmap(dpi=150)
    with open("page0.png", "wb") as f:
        f.write(png)

# Write to file directly
with flashpdf.open("paper.pdf", render_only=True) as doc:
    doc[0].get_pixmap(dpi=72, output="thumb.png")
```

For detailed render benchmarks, limitations, and local-dev workflow, see [RENDERING.md](RENDERING.md) and
[BENCHMARK_RENDER.md](BENCHMARK_RENDER.md).

---

## Output formats

### Block (text block)

```python
{
    "type": 0,                          # block type (0=text)
    "bbox": (x0, y0, x1, y1),          # bounding box in page coordinates
    "lines": [...]                      # list of lines
}
```

### Line (text line)

```python
{
    "bbox": (x0, y0, x1, y1),          # line bbox
    "spans": [...]                      # list of spans
}
```

### Span (text span)

Consecutive characters with the same font / size / color.

```python
{
    "bbox": (x0, y0, x1, y1),          # span bbox
    "text": "Hello World",              # text content
    "font": "Helvetica",                # font name
    "size": 12.0,                       # size (pt)
    "color": 0,                         # color (RGB packed)
}
```

### Image

```python
{
    "bbox": (x0, y0, x1, y1),          # position on the page
    "width": 1920,                      # pixel width
    "height": 1080,                     # pixel height
    "bpc": 8,                           # bits per channel
    "colorspace": "DeviceRGB",          # color space
    "xref": 42,                         # PDF object number
    "ext": "jpeg",                      # format: jpeg / png / jpx
    "image": b"\xff\xd8\xff...",         # raw bytes (None if include_images=False)
}
```

---

## Rust API

### `flashpdf_core::extract(path, options) -> Result<ExtractResult>`

```rust
use flashpdf_core::{extract, ExtractOptions};

let options = ExtractOptions::default();
let result = extract("document.pdf", &options)?;

for page in &result.pages {
    for block in &page.blocks {
        println!("Block: {:?}", block.bbox);
        for line in &block.lines {
            for span in &line.spans {
                println!("  [{} {:.0}pt] {}", span.font, span.size, span.text);
            }
        }
    }
    for img in &page.images {
        println!("Image: {}x{} {}", img.width, img.height, img.ext);
    }
}
```

### `flashpdf_core::extract_many(paths, options) -> Vec<(String, Result<ExtractResult>)>`

```rust
use flashpdf_core::{extract_many, ExtractOptions};

let paths = vec!["a.pdf", "b.pdf", "c.pdf"];
let options = ExtractOptions {
    file_parallel: true,
    include_images: false,
    ..Default::default()
};

for (path, result) in extract_many(&paths, &options) {
    match result {
        Ok(r) => println!("{}: {} pages", path, r.pages.len()),
        Err(e) => println!("{}: error {}", path, e),
    }
}
```

### `ExtractOptions`

```rust
pub struct ExtractOptions {
    pub page_parallel: bool,    // page-level parallelism (default true)
    pub file_parallel: bool,    // file-level parallelism (default true)
    pub include_images: bool,   // extract images (default true)
    pub gpu: bool,              // GPU acceleration (default false)
    pub batch_size: usize,      // batch size (default 50, 0=no batching)
}
```

### `Document`

Low-level document object for direct manipulation:

```rust
use flashpdf_core::Document;

let doc = Document::open("document.pdf")?;

// Page count
let count = doc.page_count()?;

// Page references
let pages = doc.page_refs()?;

// Any object
let obj = doc.get_object(42)?;

// Root catalog
let root = doc.root()?;
```

---

## Font handling

### Decode pipeline

```
Char code
  │
  ├─ 1. ToUnicode CMap lookup
  │     └─ bfchar direct mapping / bfrange range mapping
  │
  ├─ 2. Encoding Differences
  │     └─ Adobe Glyph Names in the /Differences array
  │
  ├─ 3. Raw bytes
  │     └─ ASCII (0x20-0x7E) / Latin-1 (0x80+)
  │
  └─ 4. U+FFFD (decode failure)
```

### Type0 composite fonts

Handled automatically:
- `/DescendantFonts` → CIDFont parsing
- `/W` array (both range and array forms)
- `/DW` default width
- `/CIDToGIDMap` (CIDFontType2)
- 2-byte CID code auto-detection

### Supported encodings

- Standard encodings: WinAnsiEncoding, MacRomanEncoding, MacExpertEncoding
- Differences tables
- ToUnicode CMap (bfchar + bfrange)
- Adobe Glyph List (200+ common glyphs)
- Unicode escape: `uniXXXX` format

---

## Image extraction

### Zero-copy path

JPEG and JPX images return mmap slices directly — no decode/re-encode:

```
PDF mmap → stream offset/length → return byte slice directly
```

### Lazy PNG

FlateDecode images are lazily encoded to PNG:

```
PDF mmap → FlateDecode decompress → lazy PNG encode
```

### Supported formats

| Filter | Output format | Processing |
|--------|----------|----------|
| DCTDecode | jpeg | zero-copy |
| JPXDecode | jpx | zero-copy |
| FlateDecode | png | decompress + PNG encode |
| LZWDecode | png | decompress + PNG encode |
| CCITTFaxDecode | png | decompress + PNG encode |

---

## Parallelism strategy

### Page-level parallelism (rayon)

```
PDF → page list → rayon par_iter → per-page independent extraction → merge results
```

Effective for multi-page PDFs; speedup scales with core count.

### File-level parallelism

```
[a.pdf, b.pdf, c.pdf] → rayon par_iter → per-file independent extraction
```

Effective for batch processing many files.

### Async prefetch

In sequential mode, a background thread mmaps the next file ahead of time:

```
Process file A → simultaneously mmap file B → process file B → simultaneously mmap file C → ...
```

### Large-document batching

When page count > batch_size, pages are split into batches automatically:

```
100-page PDF, batch_size=50 → batch 1 (pages 1-50) → batch 2 (pages 51-100)
```

Each batch runs independently in parallel, bounding peak memory.
