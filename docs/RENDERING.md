# Page rendering (optional `render` feature)

flashpdf is a **pure extraction library** by default — no rendering, no OCR,
no editing. Rendering is offered as an opt-in Cargo feature for users who
need `page.get_pixmap()` style page-to-image output alongside text extraction.

The rendering backend is **[PDFium]** — Google's BSD-licensed C++ PDF engine
(the same one Chrome uses to display PDFs). It is independent of flashpdf's
own parser: when you call `get_pixmap()`, PDFium re-opens the file and
rasterizes the page itself. The two engines share no internal state.

[PDFium]: https://github.com/bblanchon/pdfium-binaries

> **License note**: PDFium is BSD-3-Clause; `pdfium-render` (the Rust wrapper)
> is MIT/Apache-2.0. Both are compatible with flashpdf's MIT license. You are
> not required to change your project's license to use the `render` feature.

## Build with the feature

The default `pip install flashpdf` does **not** include rendering. To enable
it from source:

```bash
# Clone + build with the render feature
git clone https://github.com/justcodew/flashpdf.git
cd flashpdf
pip install maturin
maturin develop --release --features render
```

When the feature is **not** enabled, `page.get_pixmap()` still exists on the
`Page` object (so IDE autocomplete works), but calling it raises
`NotImplementedError` with a hint to rebuild with `--features render`.

## Install the PDFium dynamic library

PDFium is a C++ library with a stable C ABI. `pdfium-render` loads it via
dynamic FFI at runtime — the binary is **not** bundled with the wheel. You
must provide it yourself via one of:

### Option A: `PDFIUM_PATH` env var (recommended)

```bash
# 1. Download the prebuilt binary for your platform
curl -L -o /tmp/pdfium.tgz \
  https://github.com/bblanchon/pdfium-binaries/releases/latest/download/pdfium-mac.tgz

# 2. Extract
mkdir -p /tmp/pdfium && tar xzf /tmp/pdfium.tgz -C /tmp/pdfium

# 3. Point PDFIUM_PATH at the library file
#    macOS:    /tmp/pdfium/Libraries/libpdfium.dylib
#    Linux:    /tmp/pdfium/lib/libpdfium.so
#    Windows:  C:\pdfium\bin\pdfium.dll
export PDFIUM_PATH=/tmp/pdfium/Libraries/libpdfium.dylib

python -c "import flashpdf; ..."   # now get_pixmap() works
```

Available prebuilt archives (`pdfium-binaries` latest release):

| Platform | Archive | Library inside |
|---|---|---|
| macOS arm64 | `pdfium-mac.tgz` | `Libraries/libpdfium.dylib` |
| macOS x64 | `pdfium-mac-x64.tgz` | `Libraries/libpdfium.dylib` |
| Linux x64 | `pdfium-linux.tgz` | `lib/libpdfium.so` |
| Linux arm64 | `pdfium-linux-arm64.tgz` | `lib/libpdfium.so` |
| Windows x64 | `pdfium-win.tgz` | `bin\pdfium.dll` |
| Windows arm64 | `pdfium-win-arm64.tgz` | `bin\pdfium.dll` |

### Option B: `./pdfium-bin/` directory (dev convenience)

Drop the file at a known path relative to your program's working directory:

```
your-project/
├── pdfium-bin/
│   └── libpdfium.dylib    # or libpdfium.so / pdfium.dll
└── main.py
```

`pdfium-bin/` is gitignored by flashpdf — never commit the binary.

### Option C: system library

If `PDFIUM_PATH` is unset and `./pdfium-bin/` is empty, flashpdf falls back
to whatever PDFium the system loader finds (`LD_LIBRARY_PATH` / dyld /
`PATH`). This is rarely useful on dev machines but is the right knob for
package managers that install PDFium system-wide.

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

## CI / packaging (TODO)

Multi-platform wheel distribution with PDFium binaries bundled is **not yet
implemented**. The roadmap is:

1. GitHub Actions workflow that downloads `pdfium-binaries` for each target
   triple during wheel build.
2. Bundle the binary inside the wheel (platform-specific) — adds ~10MB per
   platform.
3. Expose a `pip install "flashpdf[render]"` extras that pulls the
   precompiled wheel.

Until that lands, `render` users must build from source + provide PDFium
themselves as documented above.

## Troubleshooting

**`NotImplementedError: page.get_pixmap() requires the 'render' Cargo feature`**
→ You installed the default wheel. Rebuild with
   `maturin develop --release --features render`.

**`RuntimeError: PDFium dynamic library not found. ...`**
→ PDFium binary isn't reachable. Set `PDFIUM_PATH`, drop it under
   `./pdfium-bin/`, or (Linux) install it system-wide.

**`RuntimeError: pdfium load failed for ...: ...`**
→ PDFium couldn't open the PDF. Common causes: file is corrupted, file is
   encrypted with a non-empty user password, or PDFium's version is older
   than the PDF's feature set. Try a newer `pdfium-binaries` release.
