"""Head-to-head: flashpdf vs pdf_oxide vs PyMuPDF.

Methodology mirrors pdf_oxide's published table where reasonable:
- single-thread (flashpdf page_parallel=False)
- 30 iterations, no warm-up
- text extraction only
- report mean, p99, char count

Note: pdf_oxide's corpus is 3,830 PDFs (veraPDF + pdf.js + SafeDocs).
We don't have that corpus here; this is a 2-PDF microbenchmark on academic
papers (heavier per page than the corpus average).
"""
import os
import statistics
import time

import fitz  # PyMuPDF
import pdf_oxide
import flashpdf

PDFS = [
    ("/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/dbnet_plus.pdf", "dbnet_plus"),
    ("/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/flashpdf/test_data/2604.11578v1.pdf", "arxiv_2604"),
]

ITERS = 30


def time_fn(fn):
    t0 = time.perf_counter()
    fn()
    return (time.perf_counter() - t0) * 1000  # ms


def percentile(values, p):
    s = sorted(values)
    k = int(round((p / 100.0) * (len(s) - 1)))
    return s[k]


def bench_flashpdf(path, parallel=False):
    def go():
        blocks, _imgs = flashpdf.extract(path, page_parallel=parallel, include_images=False)
        return sum(
            len(span["text"])
            for b in blocks
            for ln in b["lines"]
            for span in ln["spans"]
        )
    return go


def bench_pdf_oxide(path):
    def go():
        doc = pdf_oxide.PdfDocument(path)
        n = 0
        for page in doc:
            n += len(page.text)
        return n
    return go


def bench_pymupdf(path):
    def go():
        doc = fitz.open(path)
        n = 0
        for p in doc:
            d = p.get_text("dict")
            for b in d.get("blocks", []):
                for ln in b.get("lines", []):
                    for sp in ln.get("spans", []):
                        n += len(sp["text"])
        return n
    return go


def run(name, fn):
    samples = []
    chars = 0
    for _ in range(ITERS):
        # capture char count once
        if not samples:
            t = time_fn(fn)
            samples.append(t)
            # redo and grab chars
            t0 = time.perf_counter()
            chars = fn()
            samples[-1] = (time.perf_counter() - t0) * 1000
        else:
            samples.append(time_fn(fn))
    return {
        "mean_ms": statistics.mean(samples),
        "p99_ms": percentile(samples, 99),
        "chars": chars,
    }


def main():
    print(f"Iters per PDF: {ITERS}, single-thread, no warm-up\n")
    header = f"{'PDF':<14} {'Library':<12} {'Mean':>10} {'p99':>10} {'Chars':>10}"
    print(header)
    print("-" * len(header))
    for path, label in PDFS:
        for lib, fn_factory in [
            ("flashpdf-ST", lambda p: bench_flashpdf(p, parallel=False)),
            ("flashpdf-MT", lambda p: bench_flashpdf(p, parallel=True)),
            ("pdf_oxide", bench_pdf_oxide),
            ("PyMuPDF", bench_pymupdf),
        ]:
            r = run(label, fn_factory(path))
            print(f"{label:<14} {lib:<12} {r['mean_ms']:>8.2f}ms {r['p99_ms']:>8.2f}ms {r['chars']:>10}")
        print()


if __name__ == "__main__":
    main()
