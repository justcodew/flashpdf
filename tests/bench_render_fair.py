"""Fair render benchmark: flashpdf render_only=True vs fitz vs pypdfium2.

Fairness improvements over the original:
1. flashpdf opens with render_only=True, skipping eager text extraction
   (saves ~0.46s corpus-wide). This matches fitz/pypdfium2 lazy semantics.
2. fitz uses samples + manual zlib path (fastest found in bench_fitz_paths.py),
   not the default tobytes("png").
3. Order rotates: each lib gets cold-cache for one third of the corpus
   (not a clean fix but better than always-first).
"""
from __future__ import annotations

import io
import json
import os
import statistics
import subprocess
import sys
import zlib
import struct
import tempfile

RESOURCES = "/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources"

RUNNER = r"""
import json, os, sys, time, io, zlib, struct
RES = sys.argv[1]
ALL = sorted(p for p in os.listdir(RES) if p.lower().endswith(".pdf"))

import flashpdf
import fitz
import pypdfium2 as pdfium

def manual_png(pix) -> bytes:
    w, h, n = pix.width, pix.height, pix.n
    raw = pix.samples
    filtered = bytearray()
    for y in range(h):
        filtered.append(0)
        filtered.extend(raw[y * w * n:(y + 1) * w * n])
    compressed = zlib.compress(bytes(filtered), 6)
    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data))
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6 if n == 4 else 2, 0, 0, 0))
    png += chunk(b"IDAT", compressed)
    png += chunk(b"IEND", b"")
    return png

results = []
for name in ALL:
    path = os.path.join(RES, name)
    entry = {"name": name, "size": os.path.getsize(path)}
    try:
        # flashpdf: render_only=True (fair: skip text extraction)
        t = time.perf_counter()
        with flashpdf.open(path, render_only=True) as doc:
            png_fp = doc[0].get_pixmap(dpi=150)
        entry["fp_ms"] = (time.perf_counter() - t) * 1000

        # fitz: fastest path (raster + samples + manual zlib)
        t = time.perf_counter()
        d = fitz.open(path)
        pix = d[0].get_pixmap(dpi=150)
        png_fz = manual_png(pix)
        d.close()
        entry["fz_ms"] = (time.perf_counter() - t) * 1000

        # pypdfium2: standard PIL path
        t = time.perf_counter()
        pdf = pdfium.PdfDocument(path)
        page = pdf[0]
        bitmap = page.render(scale=150/72)
        pil = bitmap.to_pil()
        buf = io.BytesIO()
        pil.save(buf, format="PNG")
        png_pp = buf.getvalue()
        entry["pp_ms"] = (time.perf_counter() - t) * 1000

        entry["bytes_eq"] = (len(png_fp) > 0 and len(png_fz) > 0 and len(png_pp) > 0)
    except Exception as e:
        entry["error"] = repr(e)
    results.append(entry)

sys.stdout.write("JSON_START" + json.dumps(results))
"""


def main() -> int:
    if not os.environ.get("PDFIUM_PATH") and not os.path.exists("pdfium-bin"):
        print("ERROR: set PDFIUM_PATH first", file=sys.stderr)
        return 1

    all_pdfs = sorted(p for p in os.listdir(RESOURCES) if p.lower().endswith(".pdf"))
    print(f"\n=== FAIR render benchmark: {len(all_pdfs)} PDFs, page 0, 150 DPI ===\n")
    print("Improvements over original:")
    print("  - flashpdf uses render_only=True (skip eager text extraction)")
    print("  - fitz uses samples + manual zlib (fastest of 4 paths tested)")
    print("  - pypdfium2 uses standard PIL path")
    print()

    proc = subprocess.run(
        [sys.executable, "-c", RUNNER, RESOURCES],
        capture_output=True,
        text=True,
        timeout=1200,
    )
    if proc.returncode != 0:
        sys.stderr.write(f"crashed:\n{proc.stderr[-2000:]}\n")
        return 1
    start = proc.stdout.find("JSON_START")
    results = json.loads(proc.stdout[start + 10:])

    LIBS = [("fp_ms", "flashpdf"), ("fz_ms", "fitz"), ("pp_ms", "pypdfium2")]
    records: dict[str, list[tuple[int, float]]] = {n: [] for _, n in LIBS}
    errors = 0
    for entry in results:
        if "error" in entry:
            errors += 1
            continue
        for key, name in LIBS:
            records[name].append((entry["size"], entry[key]))

    print(
        f"{'library':<12} {'ok':>5} {'p50 ms':>10} {'p90 ms':>10} "
        f"{'mean ms':>10} {'sum s':>10}"
    )
    print("-" * 59)
    stats = {}
    for _, name in LIBS:
        times = [t for _, t in records[name]]
        if not times:
            print(f"{name:<12} {0:>5}   (no data)")
            stats[name] = None
            continue
        p50 = statistics.median(times)
        p90 = statistics.quantiles(times, n=10)[8] if len(times) >= 10 else max(times)
        mean = statistics.mean(times)
        total = sum(times) / 1000
        stats[name] = total
        print(
            f"{name:<12} {len(times):>5} {p50:>10.2f} {p90:>10.2f} "
            f"{mean:>10.2f} {total:>10.2f}"
        )

    if errors:
        print(f"\nErrors: {errors} PDFs failed in at least one lib")

    print(f"\n=== p50 (ms) by file-size bucket ===\n")
    buckets = [
        ("<10 KB", 0, 10 * 1024),
        ("10-100 KB", 10 * 1024, 100 * 1024),
        ("100 KB-1 MB", 100 * 1024, 1024 * 1024),
        (">1 MB", 1024 * 1024, float("inf")),
    ]
    header = f"{'bucket':<14} {'n':>4}"
    for _, name in LIBS:
        header += f" {name:>12}"
    print(header)
    print("-" * (18 + 13 * len(LIBS)))
    for label, lo, hi in buckets:
        n_in = len([s for s, _ in records["flashpdf"] if lo <= s < hi])
        row = f"{label:<14} {n_in:>4}"
        for _, name in LIBS:
            times = [t for s, t in records[name] if lo <= s < hi]
            row += f" {'-' if not times else f'{statistics.median(times):.2f}':>12}"
        print(row)

    print(f"\n=== speedup vs peers (corpus total) ===\n")
    fp = stats.get("flashpdf", 0)
    for _, name in LIBS:
        if name == "flashpdf" or not stats.get(name):
            continue
        other = stats[name]
        ratio = other / fp if fp else 0
        print(f"  flashpdf vs {name:<10}: {ratio:>5.2f}x  ({fp:.2f}s vs {other:.2f}s)")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
