"""Triage where each TIMEOUT PDF hangs.

For each known-timeout PDF, runs three phases in separate forked children
with hard SIGKILL timeouts:

  1. open(path)             — Document::from_mmap (xref parse / recovery)
  2. len(doc)               — page_refs walk or recover_page_refs scan
  3. doc[i].get_text("dict")— content stream + layout (per page)

Records which phase hangs. Lets us bucket the 36 TIMEOUTs by root cause
instead of treating them as one undifferentiated mass.
"""
from __future__ import annotations

import multiprocessing as mp
import sys
import time
from pathlib import Path

import flashpdf

CORPUS = Path("/Users/xiongzhaolong/Downloads/PyMuPDF-main/tests/resources")
PHASE_TIMEOUT = 8.0  # 8s per phase; generous for legit slow docs, catches hangs

# From bench_corpus.py output: all 36 TIMEOUT files
TIMEOUT_FILES = [
    "cms-etc-filled.pdf", "cython.pdf", "merge-form1.pdf", "merge-form2.pdf",
    "test-2333.pdf", "test-2462.pdf", "test-3150.pdf", "test-3820.pdf",
    "test-4055.pdf", "test-707673.pdf", "test-linebreaks.pdf", "test2093.pdf",
    "test_2270.pdf", "test_2548.pdf", "test_2634.pdf", "test_2791_content.pdf",
    "test_2861.pdf", "test_3357.pdf", "test_3594.pdf", "test_3677.pdf",
    "test_3725.pdf", "test_3806.pdf", "test_3842.pdf", "test_3863.pdf",
    "test_3933.pdf", "test_3950.pdf", "test_4004.pdf", "test_4034.pdf",
    "test_4047.pdf", "test_4090.pdf", "test_4388_BOZ1.pdf", "test_4388_BUL1.pdf",
    "test_4412.pdf", "test_4435.pdf", "test_4546.pdf", "test_4942.pdf",
]


def phase_open(path_str: str, q: mp.Queue) -> None:
    t0 = time.perf_counter()
    try:
        doc = flashpdf.open(path_str)
        q.put(("ok", time.perf_counter() - t0, len(doc)))
    except Exception as e:
        q.put(("err", time.perf_counter() - t0, f"{type(e).__name__}: {str(e)[:80]}"))


def phase_pagerefs(path_str: str, q: mp.Queue) -> None:
    """len(doc) triggers page_refs() — count pages without extracting text."""
    t0 = time.perf_counter()
    try:
        doc = flashpdf.open(path_str)
        n = len(doc)
        q.put(("ok", time.perf_counter() - t0, n))
    except Exception as e:
        q.put(("err", time.perf_counter() - t0, f"{type(e).__name__}: {str(e)[:80]}"))


def phase_extract(path_str: str, q: mp.Queue) -> None:
    """open + len + per-page extract_text. Hang = content stream or layout."""
    t0 = time.perf_counter()
    try:
        doc = flashpdf.open(path_str)
        n = len(doc)
        for i in range(n):
            _ = doc[i].get_text("dict")
        q.put(("ok", time.perf_counter() - t0, n))
    except Exception as e:
        q.put(("err", time.perf_counter() - t0, f"{type(e).__name__}: {str(e)[:80]}"))


def run_phase(fn, path_str: str, timeout: float):
    q: mp.Queue = mp.Queue()
    proc = mp.Process(target=fn, args=(path_str, q))
    proc.daemon = True
    proc.start()
    proc.join(timeout)
    if proc.is_alive():
        proc.terminate()
        proc.join(2)
        if proc.is_alive():
            import os
            os.kill(proc.pid, 9)
        return ("hang", timeout, None)
    if q.empty():
        return ("crash", None, None)
    try:
        return q.get_nowait()
    except Exception:
        return ("crash", None, None)


def triage(path_str: str) -> dict:
    result = {"name": Path(path_str).name, "size": Path(path_str).stat().st_size}
    # Phase 1: open alone. If hang → xref/document construction.
    r1 = run_phase(phase_open, path_str, PHASE_TIMEOUT)
    result["open"] = r1
    if r1[0] != "ok":
        result["verdict"] = "open_hang" if r1[0] == "hang" else f"open_{r1[0]}"
        return result
    # Phase 2: len(doc) — if open already worked, len triggers page_refs.
    # If hang → page tree walk or recover_page_refs scan.
    r2 = run_phase(phase_pagerefs, path_str, PHASE_TIMEOUT)
    result["pagerefs"] = r2
    if r2[0] != "ok":
        result["verdict"] = "pagerefs_hang" if r2[0] == "hang" else f"pagerefs_{r2[0]}"
        return result
    # Phase 3: full extract. If hang → content stream or layout.
    r3 = run_phase(phase_extract, path_str, PHASE_TIMEOUT)
    result["extract"] = r3
    if r3[0] != "ok":
        result["verdict"] = "extract_hang" if r3[0] == "hang" else f"extract_{r3[0]}"
        return result
    result["verdict"] = "ok"
    return result


def main():
    mp.set_start_method("fork", force=True)
    print(f"triaging {len(TIMEOUT_FILES)} TIMEOUT files, {PHASE_TIMEOUT}s per phase")
    print(f"{'file':<28} {'size':>9}  {'verdict':<16} {'detail'}")
    print("-" * 90)

    from collections import Counter
    verdicts = Counter()
    slow_ok = []

    for f in TIMEOUT_FILES:
        p = CORPUS / f
        if not p.exists():
            print(f"  {f:<28} MISSING")
            continue
        r = triage(str(p))
        v = r["verdict"]
        verdicts[v] += 1
        # Detail line: which phase, time if available, error if any
        if v == "ok":
            t = r["extract"][1]
            n = r["extract"][2]
            detail = f"{t*1000:.0f}ms, {n}p"
            if t > 2.0:
                slow_ok.append((f, t, n))
        elif v.endswith("_hang"):
            detail = f"phase='{v.split('_')[0]}'"
        else:
            phase = v.split("_")[0]
            ph_data = r.get(phase, ("", "", ""))
            detail = (ph_data[2] or "")[:60]
        print(f"  {f:<28} {r['size']:>9}  {v:<16} {detail}", flush=True)

    print("\n=== verdict tally ===")
    for v, c in verdicts.most_common():
        print(f"  {v:<20} {c}")
    if slow_ok:
        print(f"\n=== slow-but-OK (>2s) — not hangs, just heavy ===")
        for f, t, n in sorted(slow_ok, key=lambda x: -x[1])[:5]:
            print(f"  {f:<28} {t*1000:.0f}ms  {n}p")


if __name__ == "__main__":
    main()
