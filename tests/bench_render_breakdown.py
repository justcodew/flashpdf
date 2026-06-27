"""Decomposition benchmark: separate rasterization from PNG encoding.

Goal: figure out whether flashpdf's lead in render benchmarks comes from
(a) PDFium rasterizing faster than MuPDF, or (b) cheaper PNG encoding.

Each PDF gets 4 timings:
- raster_ms: pure bitmap generation (no PNG)
- png_ms: PNG encode of the resulting bitmap
- open_ms: time spent in open()/PdfDocument() before any page work

For each library we measure both phases separately.
"""
from __future__ import annotations

import json
import os
import statistics
import subprocess
import sys

RESOURCES = "/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources"

RUNNER = r"""
import json, os, sys, time
RES = sys.argv[1]
ALL = sorted(p for p in os.listdir(RES) if p.lower().endswith(".pdf"))

import flashpdf
import fitz
import pypdfium2 as pdfium
import io

results = []
for name in ALL:
    path = os.path.join(RES, name)
    entry = {"name": name, "size": os.path.getsize(path)}
    try:
        # flashpdf: open (eager extract) + get_pixmap (raster + PNG all-in-one)
        t = time.perf_counter()
        with flashpdf.open(path) as doc:
            t_open = time.perf_counter()
            png = doc[0].get_pixmap(dpi=150)
            t_done = time.perf_counter()
        entry["fp_open_ms"] = (t_open - t) * 1000
        entry["fp_render_ms"] = (t_done - t_open) * 1000  # raster + PNG combined

        # fitz: open (lazy) + rasterize only + PNG encode separately
        t = time.perf_counter()
        d = fitz.open(path)
        t_open = time.perf_counter()
        pix = d[0].get_pixmap(dpi=150)
        t_raster = time.perf_counter()
        _ = pix.tobytes("png")
        t_png = time.perf_counter()
        d.close()
        entry["fz_open_ms"] = (t_open - t) * 1000
        entry["fz_raster_ms"] = (t_raster - t_open) * 1000
        entry["fz_png_ms"] = (t_png - t_raster) * 1000

        # pypdfium2: open + render + PIL save
        t = time.perf_counter()
        pdf = pdfium.PdfDocument(path)
        t_open = time.perf_counter()
        page = pdf[0]
        bitmap = page.render(scale=150/72)
        pil = bitmap.to_pil()
        t_raster = time.perf_counter()
        buf = io.BytesIO()
        pil.save(buf, format="PNG")
        t_png = time.perf_counter()
        entry["pp_open_ms"] = (t_open - t) * 1000
        entry["pp_raster_ms"] = (t_raster - t_open) * 1000
        entry["pp_png_ms"] = (t_png - t_raster) * 1000
    except Exception as e:
        entry["error"] = repr(e)
    results.append(entry)

sys.stdout.write("JSON_START")
sys.stdout.write(json.dumps(results))
"""


def main() -> int:
    if not os.environ.get("PDFIUM_PATH") and not os.path.exists("pdfium-bin"):
        print("ERROR: set PDFIUM_PATH first", file=sys.stderr)
        return 1

    all_pdfs = sorted(p for p in os.listdir(RESOURCES) if p.lower().endswith(".pdf"))
    print(f"\n=== Render breakdown: {len(all_pdfs)} PDFs, page 0, 150 DPI ===\n")
    sys.stderr.write("running...\n")
    sys.stderr.flush()

    proc = subprocess.run(
        [sys.executable, "-c", RUNNER, RESOURCES],
        capture_output=True,
        text=True,
        timeout=1200,
    )
    if proc.returncode != 0:
        sys.stderr.write(f"runner crashed:\n{proc.stderr[-2000:]}\n")
        return 1

    start = proc.stdout.find("JSON_START")
    if start < 0:
        sys.stderr.write(f"no JSON marker in stdout:\n{proc.stdout[-500:]}\n")
        return 1
    results = json.loads(proc.stdout[start + len("JSON_START"):])

    # Bucket: median per phase per library
    phases = [
        ("fp_open_ms", "flashpdf", "open() (eager extract)"),
        ("fp_render_ms", "flashpdf", "raster+PNG"),
        ("fz_open_ms", "fitz", "open() (lazy)"),
        ("fz_raster_ms", "fitz", "raster only"),
        ("fz_png_ms", "fitz", "PNG encode"),
        ("pp_open_ms", "pypdfium2", "PdfDocument()"),
        ("pp_raster_ms", "pypdfium2", "render+to_pil"),
        ("pp_png_ms", "pypdfium2", "PIL PNG encode"),
    ]

    print(f"{'library':<12} {'phase':<25} {'p50 ms':>10} {'mean ms':>10} {'sum s':>10}")
    print("-" * 70)
    sums: dict[str, float] = {}
    for key, lib, phase in phases:
        times = [r[key] for r in results if key in r and "error" not in r]
        if not times:
            print(f"{lib:<12} {phase:<25}   (no data)")
            continue
        p50 = statistics.median(times)
        mean = statistics.mean(times)
        total = sum(times) / 1000
        sums[key] = total
        print(f"{lib:<12} {phase:<25} {p50:>10.2f} {mean:>10.2f} {total:>10.2f}")

    print()
    print("=== Component totals across corpus (sum of means, seconds) ===\n")
    print(f"  flashpdf  open+render      : {sums.get('fp_open_ms', 0) + sums.get('fp_render_ms', 0):.2f}s")
    print(f"  fitz      open+raster+png  : {sums.get('fz_open_ms', 0) + sums.get('fz_raster_ms', 0) + sums.get('fz_png_ms', 0):.2f}s")
    print(f"  pypdfium2 open+raster+png  : {sums.get('pp_open_ms', 0) + sums.get('pp_raster_ms', 0) + sums.get('pp_png_ms', 0):.2f}s")
    print()
    print(f"  fitz      open+raster only (no PNG): {sums.get('fz_open_ms', 0) + sums.get('fz_raster_ms', 0):.2f}s")
    print(f"  fitz      PNG encode only          : {sums.get('fz_png_ms', 0):.2f}s")
    print(f"  pypdfium2 raster only (no PNG)     : {sums.get('pp_open_ms', 0) + sums.get('pp_raster_ms', 0):.2f}s")
    print(f"  pypdfium2 PNG encode only          : {sums.get('pp_png_ms', 0):.2f}s")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
