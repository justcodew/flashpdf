# flashpdf

**English** · [简体中文](README.md)

**The fastest PDF library for text extraction + page rendering.** Rust core + Python bindings.

> On the 165-PDF PyMuPDF bug-regression pathological corpus:
> - Text extraction mean **6.25ms** / p50 **1.19ms** (165/165 success, 0% failure rate)
> - **2.9×** faster than pdf_oxide, **2.8×** faster than liteparse (by total time)
> - First place in every file-size bucket (tiny / small / medium / large)
>
> **Speed leader ≠ universal leader** — for encrypted PDFs (AES-256), editing,
> annotations, forms, OCR, or table extraction, PyMuPDF / pdf_oxide / pypdf /
> pdfplumber remain better fits (pick by scenario). Full comparison:
> [BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md); known shortcomings:
> [LIMITATIONS.md](docs/LIMITATIONS.md).

## Extraction demo

[Interactive page](docs/demo.html) · Example: arXiv two-column academic paper first page (title + abstract + two-column body)

![Extraction demo](docs/demo.png)

flashpdf correctly identifies the title / author line / abstract / two-column body as independent blocks, preserves font-size information, and orders blocks to match the visual reading order of the PDF.

## Installation

```bash
pip install flashpdf
```

The `pip install` wheel **bundles a PDFium binary** (~7MB), so rendering (`page.get_pixmap()`) works out of the box — no extra setup.

Build from source (requires the [Rust toolchain](https://rustup.rs)):

```bash
git clone https://github.com/justcodew/flashpdf.git
cd flashpdf && pip install maturin && maturin develop --release
```

## Quick start

```python
import flashpdf

# fitz-style (recommended): open + per-page queries
with flashpdf.open("paper.pdf") as doc:
    print(len(doc))                  # page count
    page = doc[0]                    # first page (supports doc[-1] negative indexing)
    d  = page.get_text("dict")       # structured {blocks:[...]}, text blocks type=0, image blocks type=1 inlined
    t  = page.get_text("text")       # plain-text concatenation
    bs = page.get_text("blocks")     # fitz "blocks" tuple list
    imgs = page.get_images()         # embedded images on this page
    print(page.is_scanned, page.rect, page.number)

# Functional batch entry (highest throughput for many PDFs)
for path, blocks, images in flashpdf.extract_many(
    ["a.pdf", "b.pdf", "c.pdf"],
    file_parallel=True,
):
    ...
```

### Command line (`flashpdf`)

```bash
# Extract plain text (default mode, stdout)
flashpdf extract paper.pdf

# fitz-style JSON, write to files
flashpdf extract paper.pdf --mode dict --pages 0,1,5-8 --output-dir out/

# Metadata + page overview
flashpdf info paper.pdf
flashpdf info paper.pdf --per-page      # per-page is_scanned / block count

# Table of contents (outline / TOC)
flashpdf toc paper.pdf                  # tree-indented format
flashpdf toc paper.pdf --rich           # full JSON (with kind/uri/to_point)
```

The `flashpdf` command is auto-registered by `pip install` (built on click, `[project.scripts]` entry point).


## Features

- **Extreme performance**: mmap zero-copy, memchr SIMD scanning, rayon page-level parallelism
- **Full decode pipeline**: CMap, Type0 CIDFont, Encoding Differences, embedded Type1 font /Encoding, Adobe Glyph List
- **Robust fault tolerance**: full-text scan recovery on corrupted xref; **0% failure rate** on the 165-PDF pathological corpus
- **fitz-compatible API** (v0.2.0+): `open()` / `Document` / `Page.get_text("dict"|"text"|"blocks")` align with the common PyMuPDF interface
- **Image extraction**: embedded bitmaps (JPEG/PNG/JPX) zero-copy pass-through, preserving raw bytes and 4-corner transform bbox

## Scope

flashpdf is a **read-only data extraction + optional page rendering tool** — speed is the primary goal, not universal coverage.

### ✅ Strengths (speed first)

- **Text extraction**: blocks/lines/spans with bbox/font/size/color; 0% failure rate on the 165-PDF corpus
- **Embedded image extraction**: `Do`-referenced bitmaps (JPEG/PNG/JPX) + BI/ID/EI inline images, raw bytes pass-through
- **Page rendering** (`pip install flashpdf` bundles a PDFium binary): `page.get_pixmap(dpi=150)` returns PNG bytes
- **Batch throughput**: mmap zero-copy, memchr SIMD scanning, rayon page-level + file-level parallelism

### ❌ Not supported (by design, will never be)

- **OCR**: does not recognize scanned-page text, only detects "this is a scanned page"; use Tesseract / PaddleOCR for OCR
- **PDF editing**: merge/split/add/remove pages, form filling, signing, annotations — use PyMuPDF / pypdf / pdf_oxide
- **AES-256 encrypted PDFs**: only RC4 + AES-128 with empty password are supported; AES-256 raises `ValueError`
- **Vector path extraction**: the text-extraction core does not parse path operators (curves/fill/clipping);
  PDFium rasterizes vector art fully into PNG (`get_pixmap()` output contains all vector content)
  but does not expose path coordinates/curve parameters. Use PyMuPDF's `page.get_drawings()` for vector path data
- **Accessibility tag trees (/StructTree) / embedded file streams / incremental update parsing**

### ⚠️ Possible but not best-in-class (consider switching libraries by scenario)

| Scenario | flashpdf | Better fit |
|---|---|---|
| Encrypted PDFs (any password / AES-256) | Not supported | PyMuPDF / pypdfium2 |
| PDF editing (merge / split / sign / watermark) | Not supported | pdf_oxide / PyMuPDF / pypdf |
| Rendering deployment convenience (pip install ready) | ✅ wheel bundles PDFium | pypdfium2 (also bundles PDFium) |
| Rendering raw RGBA / numpy output | Only PNG bytes | PyMuPDF (`pix.samples` direct) |
| `span.flags` accuracy | Name heuristics (does not read /FontDescriptor /Flags) | PyMuPDF |
| Font metrics fields (ascender/descender/origin) | Not output | PyMuPDF |
| Table extraction (precise cell coordinates) | Not supported | Rule-based (pdfplumber / pdftext, simple ruled tables) / Model-based (Surya / PaddleOCR PP-Structure / Table Transformer, complex / borderless / merged cells)¹ |
| LLM-friendly markdown output | Not supported | markitdown / pdftext |

¹ Table extraction has **no silver bullet** in the Python ecosystem: rule-based
libraries hit 50-90% accuracy (depending on table complexity), model-based ones
reach 90%+ but require GPU / ONNX runtime. The real reason flashpdf doesn't do
it is not "lazy" but "can't do it well and neither can others" — table
recognition is an independent subfield of layout analysis. See
[BENCHMARK_FULL.md §5](docs/BENCHMARK_FULL.md).

For the full shortcomings list (encryption restriction details, field
accuracy, untested scenarios, rendering API edges) see
**[LIMITATIONS.md](docs/LIMITATIONS.md)**; 10-library comparison see
**[BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md)**; render-only benchmark see
**[BENCHMARK_RENDER.md](docs/BENCHMARK_RENDER.md)**.

## Benchmark

> **New**: 10-library comparison (text extraction + page rendering + size buckets + selection guidance) at
> **[BENCHMARK_FULL.md](docs/BENCHMARK_FULL.md)**. Quick summary of text extraction below.

**165-PDF pathological corpus** (PyMuPDF bug-regression test set; each PDF is the minimal repro of a historical bug;
covers CJK / scanned / encrypted / tables / forms / vector art, 865B–8.3MB):

| Library | Success rate | mean | p50 | p95 | Failures |
|---|---:|---:|---:|---:|---:|
| **flashpdf** | **165/165** | **6.25ms** | **1.19ms** | **26.10ms** | **0** |
| pdf_oxide | 164/165 | 17.77ms | 1.74ms | 44.59ms | 1 (`RuntimeError`) |
| liteparse | 164/165 | 18.23ms | 1.85ms | 54.42ms | 1 (hang on `circular-toc.pdf`) |

**Total text-extraction time (sum of 165 files)**: flashpdf **1.03s** vs pdf_oxide 2.91s vs liteparse 2.99s.

**Speed multipliers (by corpus total time)**: **2.83×** vs pdf_oxide, **2.90×** vs liteparse.

**Per-file speed ratio (geo-mean, other / flashpdf)**: **1.47×** vs pdf_oxide, **1.56×** vs liteparse.

**By file-size bucket (p50 ms)**:

| Bucket | n | flashpdf | pdf_oxide | liteparse |
|---|---:|---:|---:|---:|
| tiny <10KB | 31 | **0.28** | 0.44 | 0.28 |
| small 10-100KB | 51 | **0.86** | 0.68 | 1.16 |
| medium 100KB-1MB | 63 | **2.19** | 4.58 | 4.23 |
| **large >1MB** | 20 | **8.09** | 22.09 | 18.28 |

**Conclusion**: flashpdf's advantage is largest in the large bucket (**2.3–2.7×**),
around 2× in the medium bucket; the three libraries are close in tiny/small
buckets (sub-millisecond differences, very small absolute values). flashpdf is
the top pick for "heavy-load" scenarios like RAG indexing, batch preprocessing,
and large-document parsing; for invoice/email-attachment-style small-file batch
workloads the edge is smallest but flashpdf never falls behind.

For per-file heavy-load scenarios (14–15-page arXiv papers + rayon multi-core
acceleration) flashpdf can reach even higher multipliers — this is the
best-case scenario, not the average. See [BENCHMARK.md](docs/BENCHMARK.md)
(v0.1.3 vs 10 mainstream Python PDF libraries, v0.1.x → v0.3.x stability
evolution, char-level accuracy comparison).

**Reproduce**:

```bash
git clone --depth 1 https://github.com/pymupdf/PyMuPDF.git /tmp/pymupdf
pip install flashpdf liteparse pdf-oxide pymupdf
# liteparse hangs forever on circular-toc.pdf; the script skips it
CORPUS_DIR=/tmp/pymupdf/tests/resources python tests/bench_corpus.py
```

## API reference

### `flashpdf.open(path, **options) -> Document`

fitz-style entry. `open()` performs a one-shot parallel extraction of all pages; subsequent `doc[i]` / `get_text()` calls are pure in-memory queries.

**Page methods/attributes**:

| API | Description |
|---|---|
| `page.get_text(mode)` | `"dict"` (default) / `"text"` / `"blocks"`, aligned with fitz |
| `page.get_images()` | List of embedded images on this page |
| `page.is_scanned: bool` | Scanned-page heuristic (v0.1.4) |
| `page.rect: [x0,y0,x1,y1]` | MediaBox |
| `page.number: int` | 0-based page number |
| `page.diagnostics: dict` | See [Advanced](#advanced) |

**Main options**:

| Parameter | Default | Description |
|---|---|---|
| `include_images` | `True` | Whether to extract image bytes (set `False` for text-only workloads to save decode time) |
| `include_rotated` | `False` | Whether to extract rotated/side-axis text (arXiv side-bar watermarks, chart vertical-axis labels) |
| `page_parallel` | `True` | Page-level parallelism |

### `flashpdf.extract(path, **options) -> (blocks, images[, pages])`

Functional single-file extraction. Set `with_page_info=True` to get an extra `pages` list (with `is_scanned`).

### `flashpdf.extract_many(paths, **options) -> Iterator`

Batch extraction; `file_parallel=True` is on by default.

### blocks / images structure

```python
# blocks: text blocks (in the dict mode of open(), image blocks type=1 are inlined into the same array)
{
    "type": 0,                       # 0=text, 1=image
    "bbox": (x0, y0, x1, y1),
    "lines": [{"bbox": ..., "spans": [
        {"bbox": ..., "text": "...", "font": "Helvetica",
         "size": 12.0, "color": 0, "flags": 0}   # flags: name-heuristic italic/serif/mono/bold
    ]}]
}

# images: embedded bitmaps (Do references)
{
    "bbox": (x0, y0, x1, y1),
    "width": 1920, "height": 1080,
    "colorspace": "DeviceRGB", "bpc": 8,
    "ext": "jpeg",                   # jpeg/png/jpx
    "image": b"\xff\xd8...",         # raw bytes
}
```

**fitz compatibility**: `open()` / `doc[i]` / `get_text("dict"|"text"|"blocks")` / `page.rect` /
`page.get_images()` are all aligned. Editing APIs are intentionally unsupported (see
[LIMITATIONS.md](docs/LIMITATIONS.md)).
`span.flags` uses font-name heuristics for italic/serif/mono/bold detection (does
not read `/FontDescriptor /Flags`, less accurate than fitz); fitz extension fields
like `ascender/descender/origin` are not emitted.

**Which API to use**: interactive / per-page random access → `open()`;
batch vectorization → `extract_many(file_parallel=True)`;
one-shot single file → `extract()`.

## Advanced

### Scanned-page detection (`is_scanned`)

flashpdf does not do OCR, but it can recognize scanned pages (heuristic: fewer
than 50 extractable characters on the page plus a bitmap covering ≥ 70% of the
page). Mixed documents are judged page by page:

```python
with flashpdf.open("mixed.pdf") as doc:
    for i in range(len(doc)):
        page = doc[i]
        if page.is_scanned:
            for img in page.get_images():
                your_ocr(img["image"])
        else:
            print(page.get_text("text"))
```

### Rotated-text extraction (`include_rotated`)

Characters rotated 90°/270° in a PDF (arXiv side-bar watermarks, chart
vertical-axis labels) are dropped by default — they would pollute the XY-cut
reading-order algorithm. If you need them, pass `open(path, include_rotated=True)`;
rotated characters are appended as independent blocks at the end of the page
(they do not participate in XY-cut sorting, body-character extraction is
byte-identical).

### Diagnostics (`page.diagnostics`)

Each page exposes 4 counters telling you "N characters were dropped", so you
can decide whether to re-extract or hand off to OCR. Detection always runs,
even when the corresponding toggle is off:

| Field | Meaning | Suggested follow-up |
|---|---|---|
| `rotated_char_count` | Characters under a non-axis-aligned text matrix | Re-extract with `include_rotated=True` |
| `type3_char_count` | Characters under a Type3 font | Check whether a dedicated Type 3 handler or OCR is needed |
| `undecoded_byte_count` | Bytes that failed decoding and fell back to U+FFFD | Mostly font-subsetting artifacts; OCR can recover them |
| `out_of_page_block_count` | Blocks dropped by the reading-order margin filter | Usually vector-art mis-clusters or rotated text out of bounds |
| `inline_image_count` | Inline images embedded by BI/ID/EI operators | Common in old scanned PDFs / receipts / Office exports; appear in `page.get_images()` (`name="inline"`) |

### Threading strategy (`page_parallel`)

| Mode | Use case | Notes |
|---|---|---|
| **MT** (`page_parallel=True`, default) | Single-file extraction | rayon parallelizes pages across cores; 3–4× speedup on 14–15-page heavy loads |
| **ST** (`page_parallel=False`) | `extract_many` batches | Combine with `file_parallel=True` to avoid nested rayon pools |

> All comparison libraries (pdf_oxide / PyMuPDF / pypdfium2, etc.) run
> single-threaded, so **flashpdf-ST is the apples-to-apples comparison** (still
> faster than every other library); MT is flashpdf's extra multi-core bonus.

## Architecture

```
PDF ─ mmap zero-copy
   ├─ Custom parser (objects / xref table+stream+ObjStm + memchr corruption recovery)
   ├─ Content-stream state machine (BT/ET, Tj/TJ, Td/Tm, Form XObject recursion)
   ├─ Fonts (CMap, Type0 CIDFont, Encoding, Adobe Glyph List)
   ├─ Layout (chars → spans → lines → blocks)
   └─ Images (JPEG/JPX zero-copy, FlateDecode lazy PNG, 4-corner transform bbox)
Parallelism: rayon page-level + file-level + async prefetch + large-doc batching
```

Design docs: [DESIGN_V1](docs/DESIGN_V1.md) / [DESIGN_V2](docs/DESIGN_V2.md);
full API details: [API.en.md](docs/API.en.md).

## Tests

```bash
cargo test -p flashpdf-core    # 161 core unit tests
cargo bench -p flashpdf-core   # performance benchmarks
```

## Dependencies

`memchr` (SIMD scanning) · `flate2` (zlib) · `memmap2` (mmap) · `rayon` (parallelism) ·
`pyo3` (Python bindings) · `fast-float2` · `crc32fast` · `fnv` · `smallvec`
· `pdfium-render` (optional `render` feature, PDFium rendering backend)

## Roadmap

- [x] Custom parser / content stream / fonts / layout / images / parallelism / PyPI release + CI/CD
- [x] **v0.4.0** fitz feature parity: `span.flags` · TOC · link API · CLI
- [x] **v0.5.0** broader applicability: encrypted PDFs · error messages · examples · migration guide
- [x] **v0.6.0** deeper accuracy: Type3 · vertical text · char_sim residuals
- [x] **v0.7.0** scaling: ~~expanded corpus~~ (skipped) · tiny-file perf · logging · PERFORMANCE.md
- [x] **v0.7.1** rendering: PDFium `render` feature · `get_pixmap` · `render_only`
- [x] **v0.7.2** inline images: BI/ID/EI · `inline_image_count` diagnostic
- [x] **v0.7.3** page-tree bug fix: 165/165 render zero failures (`/Prev` chain + PNG predictor + Compressed entries)
- [x] **v0.9.0** CIDFont `/W` parsing bug fix: eliminates OOM on Adobe InDesign PDFs, 165/165 text-extraction zero failures
- [x] **v0.9.1** CI / docs sync (no runtime change)

See [docs/ROADMAP.md](docs/ROADMAP.md).

## License

MIT
