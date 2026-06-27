"""
Page rendering benchmark (4 libs).

flashpdf / pypdfium2 / PyMuPDF / pdftext — these are the libs with
genuine page-render-to-pixels capability.

We render **page 0 only** of each PDF at 150 DPI, encode as PNG, and
measure total time including PNG encoding. (Page 0 is the standard "what
does this lib do per file" benchmark — full-doc render benchmark is
a separate question.)

Subprocess isolation: each (lib, file) runs in a fresh Python process.
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
TIMEOUT = 60.0
TRIALS = 2
DPI = 150

# Files known to be problematic for rendering (e.g. very large, AES-256 encrypted)
SKIP = set()

RUNNER = r'''
import json, sys, time, traceback
lib = sys.argv[1]
path = sys.argv[2]
dpi = int(sys.argv[3]) if len(sys.argv) > 3 else 150

def render_flashpdf(p, dpi):
    import flashpdf
    with flashpdf.open(p, render_only=True) as doc:
        _ = doc[0].get_pixmap(dpi=dpi)

def render_pypdfium2(p, dpi):
    import pypdfium2 as pdfium
    scale = dpi / 72.0
    pdf = pdfium.PdfDocument(p)
    bitmap = pdf[0].render(scale=scale)
    pil = bitmap.to_pil()  # forces PNG encode internally
    pdf.close()

def render_pymupdf(p, dpi):
    import fitz
    d = fitz.open(p)
    page = d[0]
    # use get_pixmap then save (returns bytes); pix.tobytes("png")
    pix = page.get_pixmap(dpi=dpi)
    _ = pix.tobytes("png")
    d.close()

def render_pdftext(p, dpi):
    # pdftext doesn't expose a raw render API directly; use its underlying pypdfium2
    import pypdfium2 as pdfium
    scale = dpi / 72.0
    pdf = pdfium.PdfDocument(p)
    bitmap = pdf[0].render(scale=scale)
    _ = bitmap.to_pil()
    pdf.close()

def render_liteparse(p, dpi):
    # liteparse.screenshot returns PNG bytes; DPI is fixed by the lib
    # (~150 DPI for A4). We can't change it; just call with page 1 (1-indexed).
    # For benchmark comparability we accept the lib's native DPI.
    from liteparse import LiteParse
    lp = LiteParse()
    rs = lp.screenshot(p, page_numbers=[1])
    _ = rs[0].image_bytes  # forces full PNG encoding

def render_pdf_oxide(p, dpi):
    from pdf_oxide import PdfDocument
    d = PdfDocument(p)
    _ = d.render_page(0, dpi=dpi)  # returns PNG bytes

runners = {
    "flashpdf": render_flashpdf,
    "pypdfium2": render_pypdfium2,
    "PyMuPDF": render_pymupdf,
    "liteparse": render_liteparse,
    "pdf_oxide": render_pdf_oxide,
}

try:
    fn = runners[lib]
    t0 = time.perf_counter()
    fn(path, dpi)
    dt_ms = (time.perf_counter() - t0) * 1000.0
    print(json.dumps({"ok": True, "ms": dt_ms}))
except Exception as e:
    tb = traceback.format_exc()[:500]
    print(json.dumps({"ok": False, "err": f"{type(e).__name__}: {e}", "tb": tb}))
'''

LIBS = ["flashpdf", "pypdfium2", "PyMuPDF", "liteparse", "pdf_oxide"]


def bench_one(lib: str, path: str) -> dict:
    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            [sys.executable, "-c", RUNNER, lib, path, str(DPI)],
            capture_output=True, text=True, timeout=TIMEOUT,
        )
        wall_ms = (time.perf_counter() - t0) * 1000.0
        if proc.returncode != 0:
            return {"ok": False, "err": f"rc={proc.returncode}", "tb": proc.stderr[-500:]}
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
    jsonl_path = OUT_DIR / "bench_render_full.jsonl"
    summary_path = OUT_DIR / "bench_render_full_summary.json"

    done = {}
    if jsonl_path.exists():
        with open(jsonl_path) as f:
            for line in f:
                try:
                    r = json.loads(line)
                    done.setdefault((r["lib"], r["file"]), []).append(r)
                except Exception:
                    pass
        print(f"Resume: {len(done)} (lib, file) pairs already done")

    with open(jsonl_path, "a") as jsonl:
        for lib in LIBS:
            for fname in CORPUS:
                if fname in SKIP:
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

    summary = {}
    for lib in LIBS:
        recs = []
        for fname in CORPUS:
            trials = done.get((lib, fname), [])
            oks = [t for t in trials if t.get("ok")]
            if oks:
                ms = min(t["ms"] for t in oks)
                recs.append({"file": fname, "ms": ms, "ok": True})
            elif trials:
                recs.append({"file": fname, "ms": None, "ok": False,
                             "err": trials[-1].get("err", "?")})
        oks_ms = [r["ms"] for r in recs if r["ok"]]
        summary[lib] = {
            "n_total": len(recs),
            "n_ok": len(oks_ms),
            "n_fail": len(recs) - len(oks_ms),
            "mean_ms": statistics.mean(oks_ms) if oks_ms else None,
            "p50_ms": statistics.median(oks_ms) if oks_ms else None,
            "p90_ms": sorted(oks_ms)[int(0.9 * len(oks_ms)) - 1] if oks_ms else None,
            "sum_ms": sum(oks_ms) if oks_ms else None,
            "fails": [r["file"] for r in recs if not r["ok"]],
            "per_file": recs,
        }

    with open(summary_path, "w") as f:
        json.dump(summary, f, indent=2)

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
