"""Re-aggregate render benchmark for 5 libs, with file-size buckets."""
import json
import os
import statistics
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "tests" / "out"
RES = Path("/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")

with open(OUT / "bench_render_full_summary.json") as f:
    render = json.load(f)

libs = ["flashpdf", "liteparse", "pypdfium2", "PyMuPDF", "pdf_oxide"]

print("=" * 92)
print("PAGE RENDERING (page 0, ~150 DPI, PNG output) — 5 libs x 165-PDF corpus")
print("=" * 92)
print(f"{'lib':12} {'ok':>5} {'fail':>5} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}")
print("-" * 92)
for lib in libs:
    d = render[lib]
    if d["mean_ms"] is None:
        print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5}  (no success)")
        continue
    print(f"{lib:12} {d['n_ok']:>5} {d['n_fail']:>5} "
          f"{d['mean_ms']:>9.2f}ms {d['p50_ms']:>9.2f}ms {d['p90_ms']:>9.2f}ms "
          f"{d['sum_ms']/1000:>8.2f}s")

# Common set
common = None
for lib in libs:
    files = {r["file"] for r in render[lib]["per_file"] if r["ok"]}
    common = files if common is None else common & files
print(f"\nFiles where all 5 libs succeeded: {len(common)}/165")

print(f"\n{'lib':12} {'mean':>11} {'p50':>11} {'p90':>11} {'sum_s':>9}  vs flashpdf")
print("-" * 80)
for lib in libs:
    times = [r["ms"] for r in render[lib]["per_file"]
             if r["ok"] and r["file"] in common]
    p90 = sorted(times)[int(0.9 * len(times)) - 1]
    sum_s = sum(times) / 1000
    fp_sum = sum(r["ms"] for r in render["flashpdf"]["per_file"]
                 if r["ok"] and r["file"] in common) / 1000
    ratio = sum_s / fp_sum if lib != "flashpdf" else 1.0
    print(f"{lib:12} {statistics.mean(times):>9.2f}ms "
          f"{statistics.median(times):>9.2f}ms {p90:>9.2f}ms "
          f"{sum_s:>8.2f}s  {ratio:.2f}x")

# File-size buckets
print("\n" + "=" * 92)
print("RENDER by file-size bucket (apples-to-apples common set)")
print("=" * 92)
sizes = {f: os.path.getsize(RES / f) for f in common}
buckets = {
    "tiny <10KB": lambda s: s < 10_000,
    "small 10-100KB": lambda s: 10_000 <= s < 100_000,
    "medium 100KB-1MB": lambda s: 100_000 <= s < 1_000_000,
    "large >1MB": lambda s: s >= 1_000_000,
}
bucket_data = {}
for bname, fn in buckets.items():
    files = {f for f in common if fn(sizes[f])}
    if not files:
        continue
    bucket_data[bname] = {"n": len(files), "libs": {}}
    print(f"\n  {bname} (n={len(files)})")
    rows = []
    for lib in libs:
        times = [r["ms"] for r in render[lib]["per_file"]
                 if r["ok"] and r["file"] in files]
        if not times:
            continue
        p50 = statistics.median(times)
        mean = statistics.mean(times)
        rows.append((lib, p50, mean))
        bucket_data[bname]["libs"][lib] = {"p50_ms": p50, "mean_ms": mean}
    rows.sort(key=lambda r: r[1])
    for lib, p50, mean in rows:
        print(f"  {lib:12} {p50:>8.2f}ms p50  {mean:>8.2f}ms mean")

out = {
    "render_libs": libs,
    "render_summary": render,
    "common_render_files": sorted(common),
    "buckets": bucket_data,
}
with open(OUT / "bench_render_aggregated.json", "w") as f:
    json.dump(out, f, indent=2)
print(f"\nWritten: {OUT/'bench_render_aggregated.json'}")
