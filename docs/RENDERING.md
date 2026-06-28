# Page rendering (optional `render` feature)

flashpdf is a **pure extraction library** by default — no rendering, no OCR,
no editing. Rendering is offered as an opt-in Cargo feature for users who
need `page.get_pixmap()` style page-to-image output alongside text extraction.

The rendering backend is **[PDFium]** — Google's BSD-3-Clause C++ PDF engine
(the same one Chrome uses to display PDFs). It is independent of flashpdf's
own parser: when you call `get_pixmap()`, PDFium re-opens the file and
rasterizes the page itself. The two engines share no internal state.

[PDFium]: https://github.com/bblanchon/pdfium-binaries

> **License note**: PDFium is BSD-3-Clause; `pdfium-render` (the Rust wrapper)
> is MIT/Apache-2.0. Both are compatible with flashpdf's MIT license. You are
> not required to change your project's license to use the `render` feature.
> See [../NOTICE](../NOTICE) for the full third-party attribution.

## Install

```bash
pip install flashpdf
```

That's it — the wheel ships PDFium bundled under `flashpdf/_pdfium/`, so
`page.get_pixmap()` works out of the box. No binary downloads, no env vars.

If you build from source without the `render` Cargo feature, `get_pixmap()`
will raise `NotImplementedError`:

```bash
# Source build with render feature enabled
git clone https://github.com/justcodew/flashpdf.git
cd flashpdf
pip install maturin
maturin develop --release --features render
```

### Binary discovery (advanced)

For completeness, when `get_pixmap()` is called, flashpdf searches for the
PDFium dynamic library in this order (first hit wins):

1. **Wheel-bundled** at `flashpdf/_pdfium/<lib>` — the default. Set
   automatically by the Python binding layer.
2. **`PDFIUM_PATH` env var** — explicit user override. Accepts either the
   library file path or its parent directory.
3. **`./pdfium-bin/<lib>`** — dev convenience, gitignored. Useful for
   running local tests against multiple PDFium versions.
4. **System library search path** — `LD_LIBRARY_PATH` / dyld / `PATH`.
   Rarely needed; useful for distro packaging.

Most users never need to touch any of these. The wheel-bundled binary is
the intended path.

## API

```python
import flashpdf

with flashpdf.open("paper.pdf") as doc:
    page = doc[0]

    # PNG bytes at 150 DPI
    png = page.get_pixmap(dpi=150)
    with open("page0.png", "wb") as f:
        f.write(png)

    # Or write directly to a path
    page.get_pixmap(dpi=300, output="page0-hires.png")
```

| Argument | Default | Notes |
|---|---|---|
| `dpi` | `150` | Output resolution. `72` = PDF native size; `300` = print quality. |
| `output` | `None` | If given, also write the PNG to this file path. |

The return value is always PNG bytes (RGBA, white background for transparency).

### Render-only fast path: `render_only=True`

If you only need rendering (no text extraction), open with `render_only=True`
to skip flashpdf's eager text+image extraction. Matches fitz / pypdfium2's
lazy `open()` semantics — saves ~3ms per PDF on average, ~13% of total time
on render-heavy workloads.

```python
# Thumbnails / OCR feedstock / batch rasterization
with flashpdf.open("big.pdf", render_only=True) as doc:
    for i in range(len(doc)):
        png = doc[i].get_pixmap(dpi=150)
        ...

# When you need BOTH text and render in the same session: use default open
with flashpdf.open("paper.pdf") as doc:
    text = doc[0].get_text("dict")
    png = doc[0].get_pixmap(dpi=150)
```

When `render_only=True`:
- OK `len(doc)`, `doc[i]`, `page.get_pixmap()` work
- Empty `get_text()` / `get_images()` / `get_links()` return empty (stubs)
- Stub `page.rect` / `page.is_scanned` are stub values
- Empty `doc.metadata` / `doc.get_toc()` return empty

### Not implemented (vs. PyMuPDF `Pixmap`)

To keep the MVP small, these fitz `get_pixmap` / `Pixmap` features are
**not** exposed:

- `clip` (sub-rectangle of the page)
- `matrix` (custom transform)
- `colorspace` / `alpha` (always RGBA, always opaque white bg)
- `Pixmap.samples` / `.tobytes()` / `.save()` (we return PNG bytes directly)
- PIL / numpy interop (decode the PNG yourself with `PIL.Image.open(io.BytesIO(png))`)

If you need any of these, please open an issue.

## Performance notes

- Each `get_pixmap()` call re-opens the file with PDFium (independent
  interpreter, no shared state with flashpdf's parser). For bulk rendering
  of all pages in a large PDF, expect roughly PyMuPDF-level throughput,
  **not** flashpdf-level text-extraction speed.
- Rendering a 100-page PDF at 150 DPI takes seconds, not milliseconds.
  flashpdf's sub-millisecond text extraction advantage does not transfer
  to rendering — that is by design (different engine, different job).
- For batch rendering workloads, prefer calling `get_pixmap()` once per
  page rather than once per pixel-level tweak.
- See [BENCHMARK_RENDER.md](BENCHMARK_RENDER.md) for measured comparisons
  against PyMuPDF and pypdfium2 (flashpdf is ~3× faster than fitz at
  corpus level thanks to a faster PNG encoder, despite using the same
  PDFium rasterizer as pypdfium2).

## Wheel size

| Wheel variant | Approx. size | Contents |
|---|---|---|
| `flashpdf` (default, render bundled) | ~10 MB | Rust ext + PDFium binary |
| Source build without `--features render` | ~3 MB | Rust ext only |

The 7 MB PDFium binary is the dominant size cost. This matches pypdfium2's
distribution model (single wheel, PDFium bundled). We chose this over
"download on first run" for offline/enterprise use cases and simplicity.

## Troubleshooting

**`NotImplementedError: page.get_pixmap() requires the 'render' Cargo feature`**
→ You built from source without the feature. Rebuild with
   `maturin develop --release --features render`. (PyPI wheels always have
   the feature on.)

**`RuntimeError: PDFium dynamic library not found. ...`**
→ Should not happen with `pip install flashpdf`. If you're using a custom
   wheel or source build, set `PDFIUM_PATH=<path>` or place the binary under
   `./pdfium-bin/`.

**`RuntimeError: pdfium load failed for ...: ...`**
→ PDFium couldn't open the PDF. Common causes: file is corrupted, file is
   encrypted with a non-empty user password, or PDFium's version is older
   than the PDF's feature set. Try a newer `pdfium-binaries` release.
