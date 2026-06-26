"""Corpus-wide benchmark: flashpdf vs liteparse vs pdf_oxide on 165 diverse PDFs.

Walks the PyMuPDF test resources directory (~165 PDFs, 865B to 8.3MB,
covering CJK / scanned / encrypted / tables / forms / academic / vector art).

Key design choices:

- **Per-file subprocess isolation.** Each (lib, file) pair runs in a forked
  child with a hard wall-clock timeout. If a lib hangs or OOMs on a single
  PDF (we observed liteparse hang on `circular-toc.pdf`), the parent kills
  the child and records TIMEOUT/CRASH. This is the only way to get a complete
  corpus run — without isolation one bad PDF kills the whole benchmark.

- **Per-file timing inside the child.** time.perf_counter() is taken inside
  the child around the lib call only, so fork/exec overhead doesn't pollute
  the measurement.

- **ONE timed parse per file.** Aggregate stats across 165 files smooth
  per-file noise; multiple iterations per file would multiply wall time.

Reports raw ms and ms/page so we can tell "which is faster" apart from
"is the speedup universal".
"""
from __future__ import annotations

import contextlib
import io
import multiprocessing as mp
import statistics
import time
from pathlib import Path

# ---- module-level so forked children can import these -----------------------
import flashpdf
import liteparse
from pdf_oxide import PdfDocument

CORPUS = Path("/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")
PER_FILE_TIMEOUT = 30.0  # seconds; liteparse hangs forever on circular-toc.pdf

BUCKETS = [
    ("tiny    <10KB", 0, 10_000),
    ("small  10-100KB", 10_000, 100_000),
    ("medium 100KB-1MB", 100_000, 1_000_000),
    ("large   >1MB", 1_000_000, float("inf")),
]


# ---- per-lib timed functions (run in child) ---------------------------------
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


FUNCS = {"fp": time_flashpdf, "lp": time_liteparse, "po": time_pdf_oxide}


# ---- child entry point ------------------------------------------------------
def _child_run(fn_key: str, path_str: str, q: mp.Queue) -> None:
    """Run inside forked child. Time the lib call, push result through queue."""
    fn = FUNCS[fn_key]
    result = fn(Path(path_str))
    try:
        q.put(result)
    except Exception:
        # Queue can break if child is in weird state; best-effort.
        q.put((None, None, "CHILD_PUT_FAILED"))


def call_with_timeout(fn_key: str, path: Path, timeout: float = PER_FILE_TIMEOUT
                      ) -> tuple[float | None, int | None, str | None]:
    """Fork a child to call FUNCS[fn_key](path); kill on timeout."""
    q: mp.Queue = mp.Queue()
    proc = mp.Process(target=_child_run, args=(fn_key, str(path), q))
    proc.daemon = True
    proc.start()
    proc.join(timeout)
    if proc.is_alive():
        proc.terminate()
        proc.join(2)
        if proc.is_alive():
            # SIGKILL if SIGTERM didn't take.
            import os
            os.kill(proc.pid, 9)
        return (None, None, "TIMEOUT")
    # Child exited. Drain queue.
    if q.empty():
        return (None, None, "CRASH")
    try:
        return q.get_nowait()
    except Exception:
        return (None, None, "CRASH")


# ---- stats helpers ----------------------------------------------------------
def pct(arr: list[float], q: float) -> float:
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
    print(f"each (lib, file) runs in a forked child with {PER_FILE_TIMEOUT:.0f}s timeout\n")

    # Use fork on POSIX so children inherit imports (cheap).
    mp.set_start_method("fork", force=True)

    rows: list[dict] = []
    t_corpus_start = time.perf_counter()
    for i, p in enumerate(pdfs):
        size = p.stat().st_size
        fp_t, fp_n, fp_err = call_with_timeout("fp", p)
        lp_t, lp_n, lp_err = call_with_timeout("lp", p)
        po_t, po_n, po_err = call_with_timeout("po", p)
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
    print(f"\ncorpus wall time: {corpus_elapsed:.1f}s ({corpus_elapsed/len(pdfs):.2f}s/file avg incl. fork overhead)")

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
    print("\n=== 解析失败/超时统计 ===")
    fp_fail = sum(1 for r in rows if r["fp_err"])
    lp_fail = sum(1 for r in rows if r["lp_err"])
    po_fail = sum(1 for r in rows if r["po_err"])
    print(f"  flashpdf : {fp_fail}/{len(rows)} failed  ({fp_fail*100/len(rows):.0f}%)")
    print(f"  liteparse: {lp_fail}/{len(rows)} failed  ({lp_fail*100/len(rows):.0f}%)")
    print(f"  pdf_oxide: {po_fail}/{len(rows)} failed  ({po_fail*100/len(rows):.0f}%)")

    # Break down by error type
    for label, key in [("flashpdf", "fp"), ("liteparse", "lp"), ("pdf_oxide", "po")]:
        errs = [r[f"{key}_err"] for r in rows if r[f"{key}_err"]]
        if not errs:
            continue
        from collections import Counter
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
