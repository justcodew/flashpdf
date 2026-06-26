"""Corpus-wide benchmark: flashpdf vs liteparse vs pdf_oxide on 165 diverse PDFs.

Walks the PyMuPDF test resources directory (~165 PDFs, 865B to 8.3MB,
covering CJK / scanned / encrypted / tables / forms / academic / vector art).

Design (v3, post-TIMEOUT-fix): **sequential inline calls**. Earlier versions
forked a subprocess per (lib, file) pair to defend against hangs — but
flashpdf v0.3.1's recovery-loop fix eliminated every TIMEOUT in the corpus,
so the isolation overhead is no longer justified. We now run each lib inline
with one known exception:

  - liteparse hangs forever on `circular-toc.pdf` (verified, lib bug).
    Hardcoded skip with a note. Every other PDF runs cleanly inline.

If a new hang is introduced (regression), the script will appear stuck —
Ctrl-C and check the last printed filename. Single timed parse per file;
aggregate stats across 165 files smooth per-file noise.
"""
from __future__ import annotations

import contextlib
import io
import statistics
import time
from pathlib import Path

import flashpdf
import liteparse
from pdf_oxide import PdfDocument

CORPUS = Path("/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")

# liteparse verified to hang indefinitely on this file (its own bug, not
# flashpdf's). Skip to keep the bench script responsive.
LITEPARSE_SKIP = {"circular-toc.pdf"}

# flashpdf v0.3.2 fixed the SIGBUS on test_3072.pdf (xy_cut infinite
# recursion); no skip is needed anymore. LITEPARSE_SKIP retained because
# liteparse's hang is upstream and not our bug to fix.

# Size buckets — does flashpdf's advantage hold on tiny PDFs vs big ones?
BUCKETS = [
    ("tiny    <10KB", 0, 10_000),
    ("small  10-100KB", 10_000, 100_000),
    ("medium 100KB-1MB", 100_000, 1_000_000),
    ("large   >1MB", 1_000_000, float("inf")),
]


def time_flashpdf(path: Path) -> tuple[float | None, int | None, str | None]:
    t0 = time.perf_counter()
    try:
        doc = flashpdf.open(str(path))
        n = len(doc)
        for i in range(n):
            _ = doc[i].get_text("dict")
        return time.perf_counter() - t0, n, None
    except Exception as e:
        return None, None, f"{type(e).__name__}: {str(e)[:80]}"


def time_liteparse(path: Path) -> tuple[float | None, int | None, str | None]:
    if path.name in LITEPARSE_SKIP:
        return None, None, "SKIP: known to hang"
    p = liteparse.LiteParse(
        ocr_enabled=False,
        output_format="markdown",
        quiet=True,
        image_mode="skip",
    )
    t0 = time.perf_counter()
    try:
        r = p.parse(str(path))
        n = len(r.pages) if hasattr(r, "pages") and r.pages else None
        for page in r.pages:
            _ = page.text
        return time.perf_counter() - t0, n, None
    except Exception as e:
        return None, None, f"{type(e).__name__}: {str(e)[:80]}"


def time_pdf_oxide(path: Path) -> tuple[float | None, int | None, str | None]:
    # pdf_oxide prints "Dictionary used where Stream expected" warnings on
    # some PDFs — silence them so they don't drown the bench output.
    with contextlib.redirect_stderr(io.StringIO()):
        t0 = time.perf_counter()
        try:
            with PdfDocument(str(path)) as doc:
                n = len(doc)
                for i in range(n):
                    _ = doc[i].text
            return time.perf_counter() - t0, n, None
        except Exception as e:
            return None, None, f"{type(e).__name__}: {str(e)[:80]}"


# ---- stats helpers ----------------------------------------------------------
def pct(arr: list[float], q: float) -> float:
    """Linear-interpolated percentile (q in [0,1])."""
    if not arr:
        return float("nan")
    a = sorted(arr)
    if len(a) == 1:
        return a[0]
    pos = q * (len(a) - 1)
    lo = int(pos)
    hi = min(lo + 1, len(a) - 1)
    frac = pos - lo
    return a[lo] * (1 - frac) + a[hi] * frac


def fmt_ms(seconds: float) -> str:
    return f"{seconds * 1000:7.2f}"


def summarize(name: str, times: list[float]) -> dict:
    return {
        "name": name,
        "count": len(times),
        "mean": statistics.mean(times) if times else float("nan"),
        "p50": pct(times, 0.50),
        "p95": pct(times, 0.95),
        "p99": pct(times, 0.99),
        "total": sum(times) if times else 0.0,
    }


def print_summary(s: dict) -> None:
    print(
        f"  {s['name']:<10}  n={s['count']:>3}  "
        f"mean={fmt_ms(s['mean'])}ms  "
        f"p50={fmt_ms(s['p50'])}ms  "
        f"p95={fmt_ms(s['p95'])}ms  "
        f"p99={fmt_ms(s['p99'])}ms  "
        f"total={fmt_ms(s['total'])}ms"
    )


# ---- main -------------------------------------------------------------------
def main() -> None:
    pdfs = sorted(CORPUS.glob("*.pdf"))
    total_bytes = sum(p.stat().st_size for p in pdfs)
    print(f"flashpdf {flashpdf.__version__}  vs  pdf_oxide  vs  liteparse")
    print(f"corpus: {len(pdfs)} PDFs, {total_bytes / 1024 / 1024:.1f} MB total")
    print(f"sequential inline calls (no subprocess isolation); "
          f"liteparse skips {len(LITEPARSE_SKIP)} known-hang file(s); "
          f"flashpdf skips none\n")

    rows: list[dict] = []
    t_corpus_start = time.perf_counter()
    for i, p in enumerate(pdfs):
        size = p.stat().st_size
        fp_t, fp_n, fp_err = time_flashpdf(p)
        lp_t, lp_n, lp_err = time_liteparse(p)
        po_t, po_n, po_err = time_pdf_oxide(p)
        status = (
            f"fp={'ERR:'+fp_err[:8] if fp_err else f'{fp_t*1000:.1f}ms':<12} "
            f"lp={'ERR:'+lp_err[:8] if lp_err else f'{lp_t*1000:.1f}ms':<12} "
            f"po={'ERR:'+po_err[:8] if po_err else f'{po_t*1000:.1f}ms':<12}"
        )
        print(f"  [{i + 1:>3}/{len(pdfs)}] {p.name[:50]:<50} {status}", flush=True)
        rows.append({
            "name": p.name, "size": size,
            "fp_t": fp_t, "fp_n": fp_n, "fp_err": fp_err,
            "lp_t": lp_t, "lp_n": lp_n, "lp_err": lp_err,
            "po_t": po_t, "po_n": po_n, "po_err": po_err,
        })

    corpus_elapsed = time.perf_counter() - t_corpus_start
    print(f"\ncorpus wall time: {corpus_elapsed:.1f}s "
          f"({corpus_elapsed/len(pdfs)*1000:.1f}ms/file avg incl. all three libs)")

    # ---- aggregate ----------------------------------------------------------
    fp_times = [r["fp_t"] for r in rows if r["fp_t"] is not None]
    lp_times = [r["lp_t"] for r in rows if r["lp_t"] is not None]
    po_times = [r["po_t"] for r in rows if r["po_t"] is not None]

    print("\n=== 全 corpus 聚合（原始 ms，仅成功文件）===")
    print_summary(summarize("flashpdf", fp_times))
    print_summary(summarize("liteparse", lp_times))
    print_summary(summarize("pdf_oxide", po_times))

    # ---- ms / page ----------------------------------------------------------
    def per_page_ms(r: dict, key: str) -> float | None:
        t = r[f"{key}_t"]
        n = r[f"{key}_n"]
        if t is None or not n:
            return None
        return t / n * 1000

    fp_pp = [x for x in (per_page_ms(r, "fp") for r in rows) if x is not None]
    lp_pp = [x for x in (per_page_ms(r, "lp") for r in rows) if x is not None]
    po_pp = [x for x in (per_page_ms(r, "po") for r in rows) if x is not None]

    print("\n=== 全 corpus 聚合（ms/页）===")
    print_summary(summarize("flashpdf", fp_pp))
    print_summary(summarize("liteparse", lp_pp))
    print_summary(summarize("pdf_oxide", po_pp))

    # ---- speedup distribution ----------------------------------------------
    def speedup_pairs(other_key: str) -> list[float]:
        out = []
        for r in rows:
            if r["fp_t"] and r[other_key + "_t"]:
                out.append(r[other_key + "_t"] / r["fp_t"])
        return out

    lp_ratio = speedup_pairs("lp")
    po_ratio = speedup_pairs("po")

    def print_ratio(name: str, arr: list[float]) -> None:
        if not arr:
            print(f"  {name}: no overlap")
            return
        print(
            f"  {name:<22}  n={len(arr):>3}  "
            f"geo-mean={statistics.geometric_mean(arr):.2f}x  "
            f"p25={pct(arr, 0.25):.2f}x  "
            f"p50={pct(arr, 0.50):.2f}x  "
            f"p75={pct(arr, 0.75):.2f}x  "
            f"min={min(arr):.2f}x  max={max(arr):.2f}x"
        )

    print("\n=== 单文件速度比（other / flashpdf；>1 表示 flashpdf 更快）===")
    print_ratio("liteparse / flashpdf", lp_ratio)
    print_ratio("pdf_oxide / flashpdf", po_ratio)

    # ---- per-bucket ---------------------------------------------------------
    print("\n=== 按文件大小分桶（fp 中位数 ms + geo-mean speedup）===")
    print(f"  {'bucket':<18} {'n':>3}  {'fp_p50':>8}  {'lp_ratio':>10}  {'po_ratio':>10}")
    for label, lo, hi in BUCKETS:
        bucket_rows = [r for r in rows if lo <= r["size"] < hi]
        if not bucket_rows:
            continue
        fp_p50 = pct([r["fp_t"] for r in bucket_rows if r["fp_t"]], 0.5) * 1000
        lp_r = [r["lp_t"] / r["fp_t"] for r in bucket_rows if r["fp_t"] and r["lp_t"]]
        po_r = [r["po_t"] / r["fp_t"] for r in bucket_rows if r["fp_t"] and r["po_t"]]
        lp_g = f"{statistics.geometric_mean(lp_r):.2f}x" if lp_r else "—"
        po_g = f"{statistics.geometric_mean(po_r):.2f}x" if po_r else "—"
        print(f"  {label:<18} {len(bucket_rows):>3}  {fp_p50:>6.2f}ms  {lp_g:>10}  {po_g:>10}")

    # ---- failures -----------------------------------------------------------
    print("\n=== 解析失败统计 ===")
    fp_fail = sum(1 for r in rows if r["fp_err"])
    lp_fail = sum(1 for r in rows if r["lp_err"])
    po_fail = sum(1 for r in rows if r["po_err"])
    print(f"  flashpdf : {fp_fail}/{len(rows)} failed  ({fp_fail*100/len(rows):.0f}%)")
    print(f"  liteparse: {lp_fail}/{len(rows)} failed  ({lp_fail*100/len(rows):.0f}%)")
    print(f"  pdf_oxide: {po_fail}/{len(rows)} failed  ({po_fail*100/len(rows):.0f}%)")

    from collections import Counter
    for label, key in [("flashpdf", "fp"), ("liteparse", "lp"), ("pdf_oxide", "po")]:
        errs = [r[f"{key}_err"] for r in rows if r[f"{key}_err"]]
        if not errs:
            continue
        c = Counter(e.split(":")[0] for e in errs)
        breakdown = ", ".join(f"{k}={v}" for k, v in c.most_common())
        print(f"    {label}: {breakdown}")

    # ---- outliers: where is flashpdf slowest? -------------------------------
    print("\n=== flashpdf 相对最慢的 5 个文件（找反例；ratio<1 表示 flashpdf 慢）===")
    for label, arr_key in [("liteparse", "lp"), ("pdf_oxide", "po")]:
        ratios = []
        for r in rows:
            if r["fp_t"] and r[arr_key + "_t"]:
                ratios.append((r[arr_key + "_t"] / r["fp_t"], r["name"]))
        ratios.sort()
        print(f"\n  vs {label}:")
        for ratio, name in ratios[:5]:
            print(f"    {ratio:.3f}x   {name}")


if __name__ == "__main__":
    main()
