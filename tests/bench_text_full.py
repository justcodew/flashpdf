"""
Comprehensive text extraction benchmark.

10 libs x 165-PDF corpus (PyMuPDF bug-regression test set).

Design:
- Each lib + file is timed in a fresh subprocess so a crash/hang in one
  combo never poisons other runs.
- 2 trials per (lib, file); take min() as the representative (best of 2
  gives a cleaner signal than median of 2).
- Per-file timeout = 60s; over-time counted as failure.
- Output: tests/out/bench_text_full.jsonl (one record per trial)
         tests/out/bench_text_full_summary.json (aggregated)
"""
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RES = Path("/System/Volumes/Data/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")
OUT_DIR = ROOT / "tests" / "out"
OUT_DIR.mkdir(parents=True, exist_ok=True)

CORPUS = sorted(p for p in os.listdir(RES) if p.lower().endswith(".pdf"))

# libs that hang/crash on specific files (skip list per lib)
SKIP = {
    "liteparse": {"circular-toc.pdf"},  # known infinite loop
}

TIMEOUT = 60.0  # seconds per (lib, file, trial)
TRIALS = 2

# Runner: a tiny standalone script that imports one lib, runs once, prints JSON
RUNNER = r'''
import json, sys, time, traceback
lib = sys.argv[1]
path = sys.argv[2]

def run_flashpdf(p):
    import flashpdf
    with flashpdf.open(p) as doc:
        n = len(doc)
        for i in range(n):
            _ = doc[i].get_text("dict")

def run_liteparse(p):
    from liteparse import LiteParse
    lp = LiteParse()
    r = lp.parse(p)

def run_pypdfium2(p):
    import pypdfium2 as pdfium
    pdf = pdfium.PdfDocument(p)
    for i in range(len(pdf)):
        tp = pdf[i].get_textpage()
        _ = tp.get_text_bounded()
    pdf.close()

def run_pdf_oxide(p):
    from pdf_oxide import PdfDocument
    d = PdfDocument(p)
    # heuristic: iterate a known page count
    # PdfDocument.page_count is the API
    n = d.page_count if hasattr(d, "page_count") else d.pages() if callable(getattr(d,"pages",None)) else 1
    if callable(n): n = n()
    for i in range(int(n)):
        try:
            _ = d.extract_text(i)
        except Exception:
            try:
                _ = d.extract_page_text(i)
            except Exception:
                pass

def run_pypdf(p):
    from pypdf import PdfReader
    r = PdfReader(p)
    for pg in r.pages:
        _ = pg.extract_text()

def run_pymupdf(p):
    import fitz
    d = fitz.open(p)
    for i in range(len(d)):
        _ = d[i].get_text("dict")
    d.close()

def run_pdftext(p):
    from pdftext.extraction import plain_text_output
    _ = plain_text_output(p)

def run_pdfminer(p):
    from pdfminer.high_level import extract_text
    _ = extract_text(p)

def run_markitdown(p):
    from markitdown import MarkItDown
    md = MarkItDown()
    _ = md.convert(p).text_content

def run_pdfplumber(p):
    import pdfplumber
    with pdfplumber.open(p) as pdf:
        for pg in pdf.pages:
            _ = pg.extract_text()

runners = {
    "flashpdf": run_flashpdf,
    "liteparse": run_liteparse,
    "pypdfium2": run_pypdfium2,
    "pdf_oxide": run_pdf_oxide,
    "pypdf": run_pypdf,
    "PyMuPDF": run_pymupdf,
    "pdftext": run_pdftext,
    "pdfminer": run_pdfminer,
    "markitdown": run_markitdown,
    "pdfplumber": run_pdfplumber,
}

try:
    fn = runners[lib]
    # warm up import (already done at module level via fn body)
    t0 = time.perf_counter()
    fn(path)
    dt_ms = (time.perf_counter() - t0) * 1000.0
    print(json.dumps({"ok": True, "ms": dt_ms}))
except Exception as e:
    tb = traceback.format_exc()[:500]
    print(json.dumps({"ok": False, "err": f"{type(e).__name__}: {e}", "tb": tb}))
'''

LIBS = [
    "flashpdf", "liteparse", "pypdfium2", "pdf_oxide", "pypdf",
    "PyMuPDF", "pdftext", "pdfminer", "markitdown", "pdfplumber",
]


def bench_one(lib: str, path: str) -> dict:
    """Run one (lib, file) in a subprocess and return result dict."""
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            [sys.executable, "-c", RUNNER, lib, path],
            capture_output=True, text=True, timeout=TIMEOUT,
        )
        wall_ms = (time.perf_counter() - t0) * 1000.0
        if proc.returncode != 0:
            return {"ok": False, "err": f"rc={proc.returncode}", "tb": proc.stderr[-500:]}
        # last non-empty line of stdout is the JSON
        out_lines = [l for l in proc.stdout.strip().splitlines() if l.strip()]
        if not out_lines:
            return {"ok": False, "err": "no stdout", "tb": proc.stderr[-500:]}
        result = json.loads(out_lines[-1])
        result["wall_ms"] = wall_ms
        return result
    except subprocess.TimeoutExpired:
        return {"ok": False, "err": f"timeout>{TIMEOUT}s"}
    except Exception as e:
        return {"ok": False, "err": f"{type(e).__name__}: {e}"}


def main():
    jsonl_path = OUT_DIR / "bench_text_full.jsonl"
    summary_path = OUT_DIR / "bench_text_full_summary.json"

    # Load existing results so we can resume
    done = {}  # (lib, file) -> [ms, ms, ...]
    if jsonl_path.exists():
        with open(jsonl_path) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    key = (r["lib"], r["file"])
                    done.setdefault(key, []).append(r)
                except Exception:
                    pass
        print(f"Resume: {len(done)} (lib, file) pairs already done")

    with open(jsonl_path, "a") as jsonl:
        for lib in LIBS:
            skip_set = SKIP.get(lib, set())
            for fname in CORPUS:
                if fname in skip_set:
                    continue
                key = (lib, fname)
                trials = done.get(key, [])
                need = max(0, TRIALS - len(trials))
                if need == 0:
                    continue
                path = str(RES / fname)
                for _ in range(need):
                    res = bench_one(lib, path)
                    rec = {"lib": lib, "file": fname, "size": os.path.getsize(path)}
                    rec.update(res)
                    jsonl.write(json.dumps(rec) + "\n")
                    jsonl.flush()
                    done.setdefault(key, []).append(rec)
                    status = "ok" if res.get("ok") else f"FAIL:{res.get('err','?')[:30]}"
                    print(f"  {lib:12} {fname:40} {res.get('ms', res.get('wall_ms', -1)):8.2f}ms  {status}")

    # Aggregate
    summary = {}
    for lib in LIBS:
        recs = []
        for fname in CORPUS:
            trials = done.get((lib, fname), [])
            oks = [t for t in trials if t.get("ok")]
            if oks:
                # min of trial times
                ms = min(t["ms"] for t in oks)
                recs.append({"file": fname, "ms": ms, "ok": True})
            elif trials:
                recs.append({"file": fname, "ms": None, "ok": False,
                             "err": trials[-1].get("err", "?")})
        oks_ms = [r["ms"] for r in recs if r["ok"]]
        n_ok = len(oks_ms)
        n_fail = len(recs) - n_ok
        summary[lib] = {
            "n_total": len(recs),
            "n_ok": n_ok,
            "n_fail": n_fail,
            "mean_ms": statistics.mean(oks_ms) if oks_ms else None,
            "p50_ms": statistics.median(oks_ms) if oks_ms else None,
            "p90_ms": sorted(oks_ms)[int(0.9 * len(oks_ms)) - 1] if oks_ms else None,
            "sum_ms": sum(oks_ms) if oks_ms else None,
            "fails": [r["file"] for r in recs if not r["ok"]],
            "per_file": recs,
        }

    with open(summary_path, "w") as f:
        json.dump(summary, f, indent=2)

    # Print table
    print("\n" + "=" * 90)
    print(f"{'lib':12} {'ok':>5} {'fail':>5} {'mean':>10} {'p50':>10} {'p90':>10} {'sum_s':>10}")
    print("-" * 90)
    for lib in LIBS:
        s = summary[lib]
        if s["mean_ms"] is None:
            print(f"{lib:12} {s['n_ok']:>5} {s['n_fail']:>5}  (no successful runs)")
            continue
        print(f"{lib:12} {s['n_ok']:>5} {s['n_fail']:>5} "
              f"{s['mean_ms']:>9.2f}ms {s['p50_ms']:>9.2f}ms {s['p90_ms']:>9.2f}ms "
              f"{s['sum_ms']/1000:>9.2f}s")
    print("=" * 90)
    print(f"\nDetailed JSON: {summary_path}")


if __name__ == "__main__":
    main()
