"""Generate final aggregated analysis from bench_text_full + bench_render_full."""
import json
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "tests" / "out"

with open(OUT / "bench_text_full_summary.json") as f:
    text = json.load(f)
with open(OUT / "bench_render_full_summary.json") as f:
    render = json.load(f)

# === TEXT ===
print("=" * 92)
print("TEXT EXTRACTION — 10 libs x 165-PDF corpus")
print("=" * 92)
text_libs = ["flashpdf", "liteparse", "pdf_oxide", "pypdfium2", "PyMuPDF",
             "pypdf", "pdftext", "pdfminer", "pdfplumber", "markitdown"]
print(f"{'lib':12} {'ok':>5} {'fail':>5} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}")
print("-" * 92)
for lib in text_libs:
    d = text[lib]
    if d["mean_ms"] is None:
        print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5}  (no success)")
        continue
    print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5} "
          f"{d['mean_ms']:>9.2f}ms {d['p50_ms']:>9.2f}ms {d['p90_ms']:>9.2f}ms "
          f"{d['sum_ms']/1000:>8.2f}s")

# Apples-to-apples: only files where ALL libs succeeded
common_text = None
for lib in text_libs:
    files = {r["file"] for r in text[lib]["per_file"] if r["ok"]}
    common_text = files if common_text is None else common_text & files
print(f"\nFiles where all 10 libs succeeded: {len(common_text)}/165")

# Re-aggregate on common set
print(f"\n{'lib':12} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}")
print("-" * 70)
for lib in text_libs:
    times = [r["ms"] for r in text[lib]["per_file"]
             if r["ok"] and r["file"] in common_text]
    if not times:
        continue
    p90 = sorted(times)[int(0.9 * len(times)) - 1]
    print(f"{lib:12} {statistics.mean(times):>9.2f}ms "
          f"{statistics.median(times):>9.2f}ms {p90:>9.2f}ms "
          f"{sum(times)/1000:>8.2f}s")

# === RENDER ===
print("\n" + "=" * 92)
print("PAGE RENDERING (page 0, DPI=150, PNG output) — 3 libs x 165-PDF corpus")
print("=" * 92)
render_libs = ["flashpdf", "pypdfium2", "PyMuPDF"]
print(f"{'lib':12} {'ok':>5} {'fail':>5} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}")
print("-" * 92)
for lib in render_libs:
    d = render[lib]
    if d["mean_ms"] is None:
        print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5}  (no success)")
        continue
    print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5} "
          f"{d['mean_ms']:>9.2f}ms {d['p50_ms']:>9.2f}ms {d['p90_ms']:>9.2f}ms "
          f"{d['sum_ms']/1000:>8.2f}s")

common_render = None
for lib in render_libs:
    files = {r["file"] for r in render[lib]["per_file"] if r["ok"]}
    common_render = files if common_render is None else common_render & files
print(f"\nFiles where all 3 render libs succeeded: {len(common_render)}/165")

print(f"\n{'lib':12} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}")
print("-" * 70)
for lib in render_libs:
    times = [r["ms"] for r in render[lib]["per_file"]
             if r["ok"] and r["file"] in common_render]
    p90 = sorted(times)[int(0.9 * len(times)) - 1]
    print(f"{lib:12} {statistics.mean(times):>9.2f}ms "
          f"{statistics.median(times):>9.2f}ms {p90:>9.2f}ms "
          f"{sum(times)/1000:>8.2f}s")

# === FILE-SIZE BUCKETS (text extraction) ===
print("\n" + "=" * 92)
print("TEXT EXTRACTION by file-size bucket (apples-to-apples, n files where all libs ok)")
print("=" * 92)
import os
RES = Path("/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")
sizes = {f: os.path.getsize(RES / f) for f in common_text}
buckets = {
    "tiny <10KB": lambda s: s < 10_000,
    "small 10-100KB": lambda s: 10_000 <= s < 100_000,
    "medium 100KB-1MB": lambda s: 100_000 <= s < 1_000_000,
    "large >1MB": lambda s: s >= 1_000_000,
}
for bname, fn in buckets.items():
    files = {f for f in common_text if fn(sizes[f])}
    if not files:
        continue
    print(f"\n  {bname} (n={len(files)})")
    print(f"  {'lib':12} {'p50':>10}")
    rows = []
    for lib in text_libs:
        times = [r["ms"] for r in text[lib]["per_file"]
                 if r["ok"] and r["file"] in files]
        if not times:
            continue
        rows.append((lib, statistics.median(times), statistics.mean(times)))
    rows.sort(key=lambda r: r[1])
    for lib, p50, mean in rows:
        print(f"  {lib:12} {p50:>8.2f}ms")

# Save bucketed data for the markdown report
bucket_data = {}
for bname, fn in buckets.items():
    files = {f for f in common_text if fn(sizes[f])}
    bucket_data[bname] = {"n": len(files), "libs": {}}
    for lib in text_libs:
        times = [r["ms"] for r in text[lib]["per_file"]
                 if r["ok"] and r["file"] in files]
        if times:
            bucket_data[bname]["libs"][lib] = {
                "p50_ms": statistics.median(times),
                "mean_ms": statistics.mean(times),
                "p90_ms": sorted(times)[int(0.9*len(times))-1],
            }

out = {
    "text_libs": text_libs,
    "render_libs": render_libs,
    "text_summary": text,
    "render_summary": render,
    "common_text_files": sorted(common_text),
    "common_render_files": sorted(common_render),
    "buckets": bucket_data,
}
with open(OUT / "bench_aggregated.json", "w") as f:
    json.dump(out, f, indent=2)
print(f"\nWritten: {OUT/'bench_aggregated.json'}")
