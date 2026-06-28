"""Compare fitz's PNG output paths to find the fastest one.

Paths:
1. pix.tobytes("png") — current default in our benchmark
2. pix.save(path) — direct file write (skip Python bytes roundtrip)
3. pix.samples + zlib + manual PNG — bypass fitz's encoder entirely
4. pix.pil_tobytes(format="PNG") — fitz's PIL bridge
"""
from __future__ import annotations

import io
import os
import statistics
import subprocess
import sys
import json

RESOURCES = "/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources"

RUNNER = r"""
import json, os, sys, time
RES = sys.argv[1]
ALL = sorted(p for p in os.listdir(RES) if p.lower().endswith(".pdf"))

import fitz
import io
import zlib
import struct

results = []
for name in ALL:
    path = os.path.join(RES, name)
    entry = {"name": name, "size": os.path.getsize(path)}
    try:
        doc = fitz.open(path)
        page = doc[0]
        # 1. tobytes("png")
        t = time.perf_counter()
        pix = page.get_pixmap(dpi=150)
        t_raster_done = time.perf_counter()
        _ = pix.tobytes("png")
        t_tobytes = time.perf_counter()
        # 2. save() to /tmp file
        import tempfile
        f = tempfile.NamedTemporaryFile(suffix=".png", delete=False)
        f.close()
        pix.save(f.name)
        t_save = time.perf_counter()
        os.unlink(f.name)
        # 3. samples + manual PNG encode via zlib
        # PNG with IDAT-only, no filtering (raw bytes with filter byte 0 per row)
        w, h = pix.width, pix.height
        n = pix.n  # components
        raw = pix.samples
        # Add filter byte 0 at start of each row
        filtered = bytearray()
        for y in range(h):
            filtered.append(0)
            filtered.extend(raw[y * w * n:(y + 1) * w * n])
        compressed = zlib.compress(bytes(filtered), 6)
        # Build minimal PNG
        def chunk(typ: bytes, data: bytes) -> bytes:
            c = struct.pack(">I", len(data)) + typ + data
            crc = zlib.crc32(typ + data)
            return c + struct.pack(">I", crc)
        png = b"\x89PNG\r\n\x1a\n"
        ihdr = struct.pack(">IIBBBBB", w, h, 8, 6 if n == 4 else 2, 0, 0, 0)
        png += chunk(b"IHDR", ihdr)
        png += chunk(b"IDAT", compressed)
        png += chunk(b"IEND", b"")
        t_manual = time.perf_counter()
        doc.close()
        entry["tobytes_ms"] = (t_tobytes - t_raster_done) * 1000
        entry["save_ms"] = (t_save - t_tobytes) * 1000
        entry["manual_ms"] = (t_manual - t_save) * 1000
        entry["raster_ms"] = (t_raster_done - t) * 1000
        entry["png_size"] = len(_)
    except Exception as e:
        entry["error"] = repr(e)
    results.append(entry)

sys.stdout.write("JSON_START" + json.dumps(results))
"""


def main() -> int:
    print(f"\n=== fitz PNG path comparison ({len(os.listdir(RESOURCES))} PDFs, page 0, 150 DPI) ===\n")
    proc = subprocess.run(
        [sys.executable, "-c", RUNNER, RESOURCES],
        capture_output=True,
        text=True,
        timeout=600,
    )
    if proc.returncode != 0:
        sys.stderr.write(f"crashed:\n{proc.stderr[-2000:]}\n")
        return 1
    start = proc.stdout.find("JSON_START")
    results = json.loads(proc.stdout[start + 10:])

    phases = [
        ("raster_ms", "raster only (get_pixmap)"),
        ("tobytes_ms", "pix.tobytes('png')"),
        ("save_ms", "pix.save(path)"),
        ("manual_ms", "samples + zlib + manual"),
    ]

    print(f"{'phase':<32} {'p50 ms':>10} {'mean ms':>10} {'sum s':>10}")
    print("-" * 65)
    for key, label in phases:
        times = [r[key] for r in results if key in r and "error" not in r]
        if not times:
            continue
        p50 = statistics.median(times)
        mean = statistics.mean(times)
        total = sum(times) / 1000
        print(f"{label:<32} {p50:>10.2f} {mean:>10.2f} {total:>10.2f}")

    print()
    print("=== Totals (open+raster+encode) ===")
    raster_sum = sum(r.get("raster_ms", 0) for r in results if "error" not in r) / 1000
    tobytes_sum = sum(r.get("tobytes_ms", 0) for r in results if "error" not in r) / 1000
    save_sum = sum(r.get("save_ms", 0) for r in results if "error" not in r) / 1000
    manual_sum = sum(r.get("manual_ms", 0) for r in results if "error" not in r) / 1000
    print(f"  raster only           : {raster_sum:.2f}s")
    print(f"  raster + tobytes      : {raster_sum + tobytes_sum:.2f}s")
    print(f"  raster + save         : {raster_sum + save_sum:.2f}s")
    print(f"  raster + manual zlib  : {raster_sum + manual_sum:.2f}s")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
