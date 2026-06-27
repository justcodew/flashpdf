"""Render-speed benchmark: flashpdf vs fitz vs pypdfium2.

Methodology: 165-PDF PyMuPDF corpus, render page 0 at 150 DPI.

Each library runs in its own subprocess to isolate PDFium's process-wide
singleton. The runner imports the lib, iterates the corpus, and times each
render end-to-end (open + render + PNG encode).
"""
from __future__ import annotations

import json
import os
import statistics
import subprocess
import sys
import textwrap

RESOURCES = "/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources"

# Runner writes JSON to stdout: list of {"name", "ms", "bytes"} or {"name", "error"}.
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
    try:
        # flashpdf
        t = time.perf_counter()
        with flashpdf.open(path) as doc:
            png_fp = doc[0].get_pixmap(dpi=150)
        dt_fp = (time.perf_counter() - t) * 1000

        # fitz
        t = time.perf_counter()
        d = fitz.open(path)
        pix = d[0].get_pixmap(dpi=150)
        png_fz = pix.tobytes('png')
        d.close()
        dt_fz = (time.perf_counter() - t) * 1000

        # pypdfium2
        t = time.perf_counter()
        pdf = pdfium.PdfDocument(path)
        page = pdf[0]
        bitmap = page.render(scale=150/72)
        pil = bitmap.to_pil()
        buf = io.BytesIO()
        pil.save(buf, format='PNG')
        png_pp = buf.getvalue()
        dt_pp = (time.perf_counter() - t) * 1000

        results.append({
            "name": name, "size": os.path.getsize(path),
            "fp_ms": dt_fp, "fp_bytes": len(png_fp),
            "fz_ms": dt_fz, "fz_bytes": len(png_fz),
            "pp_ms": dt_pp, "pp_bytes": len(png_pp),
        })
    except Exception as e:
        results.append({"name": name, "size": os.path.getsize(path), "error": repr(e)})

sys.stdout.write(json.dumps(results))
"""


def main() -> int:
    if not os.environ.get("PDFIUM_PATH") and not os.path.exists("pdfium-bin"):
        print("ERROR: set PDFIUM_PATH first", file=sys.stderr)
        return 1

    all_pdfs = sorted(p for p in os.listdir(RESOURCES) if p.lower().endswith(".pdf"))
    print(f"\n=== Render benchmark: {len(all_pdfs)} PDFs, page 0, 150 DPI ===\n")
    sys.stderr.write("running (in-process per-PDF, 3 libs sequentially)...\n")
    sys.stderr.flush()

    proc = subprocess.run(
        [sys.executable, "-c", RUNNER, RESOURCES],
        capture_output=True,
        text=True,
        timeout=1200,
    )
    if proc.returncode != 0:
        sys.stderr.write(f"runner crashed:\n{proc.stderr[-3000:]}\n")
        return 1

    try:
        # fitz prints "MuPDF error: ..." to stdout; strip everything before
        # the first JSON array opening bracket.
        json_start = proc.stdout.find("[")
        results = json.loads(proc.stdout[json_start:])
    except (json.JSONDecodeError, ValueError) as e:
        sys.stderr.write(f"JSON parse failed: {e}\n{proc.stdout[-500:]}\n")
        return 1

    LIBS = ["fp_ms", "fz_ms", "pp_ms"]
    NAMES = {"fp_ms": "flashpdf", "fz_ms": "fitz", "pp_ms": "pypdfium2"}

    records: dict[str, list[tuple[int, float]]] = {n: [] for n in LIBS}
    errors: list[str] = []
    for entry in results:
        if "error" in entry:
            errors.append(entry["name"])
            continue
        for key in LIBS:
            records[key].append((entry["size"], entry[key]))

    print(
        f"{'library':<12} {'ok':>5} {'p50 ms':>10} {'p90 ms':>10} "
        f"{'mean ms':>10} {'sum s':>10}"
    )
    print("-" * 59)
    stats = {}
    for key in LIBS:
        times = [t for _, t in records[key]]
        if not times:
            print(f"{NAMES[key]:<12} {0:>5}   (no data)")
            stats[key] = None
            continue
        p50 = statistics.median(times)
        p90 = statistics.quantiles(times, n=10)[8] if len(times) >= 10 else max(times)
        mean = statistics.mean(times)
        total = sum(times) / 1000
        stats[key] = {"p50": p50, "p90": p90, "mean": mean, "total": total}
        print(
            f"{NAMES[key]:<12} {len(times):>5} {p50:>10.2f} {p90:>10.2f} "
            f"{mean:>10.2f} {total:>10.2f}"
        )

    if errors:
        print(f"\nErrors: {len(errors)} PDFs failed in at least one lib: {errors[:5]}")

    print(f"\n=== p50 (ms) by file-size bucket ===\n")
    buckets = [
        ("<10 KB", 0, 10 * 1024),
        ("10-100 KB", 10 * 1024, 100 * 1024),
        ("100 KB-1 MB", 100 * 1024, 1024 * 1024),
        (">1 MB", 1024 * 1024, float("inf")),
    ]
    header = f"{'bucket':<14} {'n':>4}"
    for key in LIBS:
        header += f" {NAMES[key]:>12}"
    print(header)
    print("-" * (18 + 13 * len(LIBS)))
    for label, lo, hi in buckets:
        n_in = len([s for s, _ in records["fp_ms"] if lo <= s < hi])
        row = f"{label:<14} {n_in:>4}"
        for key in LIBS:
            times = [t for s, t in records[key] if lo <= s < hi]
            row += f" {'-' if not times else f'{statistics.median(times):.2f}':>12}"
        print(row)
    print()

    print(f"=== speedup vs peers (corpus total) ===\n")
    fp_total = stats["fp_ms"]["total"] if stats.get("fp_ms") else 0
    for key in LIBS:
        if key == "fp_ms" or not stats.get(key):
            continue
        other = stats[key]["total"]
        ratio = other / fp_total if fp_total else 0
        print(
            f"  flashpdf vs {NAMES[key]:<10}: {ratio:>5.2f}x  "
            f"({fp_total:.2f}s vs {other:.2f}s)"
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
