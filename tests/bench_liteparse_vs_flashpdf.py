"""Benchmark flashpdf vs liteparse vs pdf_oxide on representative PDFs.

Runs each library N times, reports mean / p50 / p99 wall-clock.
All libs are warmed up once before measurement to factor out import/disk cache.
"""
import contextlib
import io
import statistics
import time
from pathlib import Path

import flashpdf
import liteparse
import pdf_oxide  # noqa: F401  — register module-level import for fairness
from pdf_oxide import PdfDocument

ROOT = Path(__file__).parent.parent
ARXIV = ROOT / "test_data" / "2604.11578v1.pdf"
DBNET = Path("/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/dbnet_plus.pdf")

# (label, path, runs) — fewer runs for the big doc to keep total wall time sane
TARGETS = [
    ("arxiv_2604 (14p, 1.3MB, text-heavy)", ARXIV, 8),
    ("dbnet_plus (15p, 6.4MB, image-heavy)", DBNET, 5),
]


def time_flashpdf(path: Path, include_images: bool) -> float:
    t0 = time.perf_counter()
    doc = flashpdf.open(str(path), include_images=include_images)
    # Materialize per-page dicts so we measure the same work flashpdf would do
    # in a real pipeline (not just the lazy Arc clone).
    for i in range(len(doc)):
        _ = doc[i].get_text("dict")
    return time.perf_counter() - t0


def time_liteparse(path: Path) -> float:
    p = liteparse.LiteParse(
        ocr_enabled=False,
        output_format="markdown",
        quiet=True,
        image_mode="skip",  # match flashpdf include_images=False for apples-to-apples
    )
    t0 = time.perf_counter()
    r = p.parse(str(path))
    # Materialize text per page — liteparse gives this eagerly, but iterate to be safe.
    for page in r.pages:
        _ = page.text
    return time.perf_counter() - t0


def time_pdf_oxide(path: Path) -> float:
    # pdf_oxide prints "Dictionary used where Stream expected" warnings to stderr
    # on some PDFs — silence them so they don't muddy the bench output.
    with contextlib.redirect_stderr(io.StringIO()):
        t0 = time.perf_counter()
        with PdfDocument(str(path)) as doc:
            for i in range(len(doc)):
                _ = doc[i].text  # lazy property — materialize to match peers
    return time.perf_counter() - t0


def fmt(samples: list[float]) -> str:
    samples = sorted(samples)
    n = len(samples)
    mean = statistics.mean(samples)
    p50 = samples[n // 2]
    p99_idx = max(0, int(n * 0.99) - 1)
    p99 = samples[min(p99_idx, n - 1)]
    return f"mean={mean*1000:6.1f}ms  p50={p50*1000:6.1f}ms  p99={p99*1000:6.1f}ms"


def bench(label: str, path: Path, runs: int) -> None:
    print(f"\n=== {label} ===")
    print(f"  pdf: {path.name}")

    # Warmup: 1 run each, discarded. Factors out disk cache + JIT-ish first-run overhead.
    _ = time_flashpdf(path, include_images=False)
    _ = time_liteparse(path)
    _ = time_pdf_oxide(path)

    fp_no_img = [time_flashpdf(path, include_images=False) for _ in range(runs)]
    fp_img = [time_flashpdf(path, include_images=True) for _ in range(runs)]
    lp = [time_liteparse(path) for _ in range(runs)]
    po = [time_pdf_oxide(path) for _ in range(runs)]

    print(f"  flashpdf   (include_images=False):  {fmt(fp_no_img)}")
    print(f"  flashpdf   (include_images=True):   {fmt(fp_img)}")
    print(f"  pdf_oxide  (.text per page):        {fmt(po)}")
    print(f"  liteparse  (image_mode=skip):       {fmt(lp)}")

    # Speedup ratios (mean), flashpdf as baseline.
    m_fp = statistics.mean(fp_no_img)
    m_po = statistics.mean(po)
    m_lp = statistics.mean(lp)
    if m_fp > 0:
        if m_po > 0:
            r = m_po / m_fp
            print(
                f"  → flashpdf (no img) is {r:.2f}× "
                f"{'faster' if r > 1 else 'slower'} than pdf_oxide"
            )
        if m_lp > 0:
            r = m_lp / m_fp
            print(
                f"  → flashpdf (no img) is {r:.2f}× "
                f"{'faster' if r > 1 else 'slower'} than liteparse"
            )


def main() -> None:
    print(f"flashpdf {flashpdf.__version__}  vs  pdf_oxide 0.3.68  vs  liteparse 2.2.1")
    print(f"runs per target: warm-up discarded, then N timed runs")
    for label, path, runs in TARGETS:
        if not path.exists():
            print(f"\n[skip] {path} not found")
            continue
        bench(label, path, runs)


if __name__ == "__main__":
    main()
