"""Head-to-head: flashpdf vs the 8 Python PDF libraries from pdf_oxide's published table.

Libraries tested (matching pdf_oxide's README):
  - flashpdf (ours, multi-thread + single-thread variants)
  - pdf_oxide
  - PyMuPDF (fitz)
  - pypdfium2
  - pymupdf4llm
  - pdftext
  - pdfminer.six
  - pdfplumber
  - markitdown
  - pypdf

Methodology:
  - 30 iterations per (library, PDF), 1 warm-up call, record mean / p99
  - text extraction only (no image extraction)
  - macOS Apple Silicon, Python 3.14, single-thread except flashpdf-MT

Note on "chars" column:
  - flashpdf / PyMuPDF / pypdfium2 / pdfminer / pdfplumber / pypdf: plain text length
  - pymupdf4llm / pdftext / markitdown: markdown output (includes #, *, \\n, etc.)
  - pdf_oxide: page.text concatenated (includes newlines)
  Numbers are not directly comparable across output formats; speed is.
"""
import os
import statistics
import time

PDFS = [
    ("/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/dbnet_plus.pdf", "dbnet_plus"),
    ("/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/flashpdf/test_data/2604.11578v1.pdf", "arxiv_2604"),
]

ITERS = 30


def percentile(values, p):
    s = sorted(values)
    k = int(round((p / 100.0) * (len(s) - 1)))
    return s[k]


def time_fn(fn):
    t0 = time.perf_counter()
    result = fn()
    return (time.perf_counter() - t0) * 1000, result


def run(fn, iters=ITERS):
    # 1 warm-up call (untimed)
    chars = fn()
    samples = []
    for _ in range(iters):
        ms, chars = time_fn(fn)
        samples.append(ms)
    return {
        "mean_ms": statistics.mean(samples),
        "p99_ms": percentile(samples, 99),
        "chars": chars,
    }


# --- per-library adapters ----------------------------------------------

def fn_flashpdf(path, parallel):
    import flashpdf
    def go():
        blocks, _ = flashpdf.extract(path, page_parallel=parallel, include_images=False)
        return sum(len(sp["text"]) for b in blocks for ln in b["lines"] for sp in ln["spans"])
    return go


def fn_pdf_oxide(path):
    import pdf_oxide
    def go():
        doc = pdf_oxide.PdfDocument(path)
        n = 0
        for page in doc:
            n += len(page.text)
        return n
    return go


def fn_pymupdf(path):
    import fitz
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


def fn_pypdfium2(path):
    import pypdfium2 as pdfium
    def go():
        doc = pdfium.PdfDocument(path)
        n = 0
        for i in range(len(doc)):
            page = doc[i]
            tp = page.get_textpage()
            n += len(tp.get_text_bounded())
            tp.close()
            page.close()
        doc.close()
        return n
    return go


def fn_pymupdf4llm(path):
    import pymupdf4llm
    def go():
        md = pymupdf4llm.to_markdown(path)
        return len(md)
    return go


def fn_pdftext(path):
    from pdftext.extraction import plain_text_output
    def go():
        return len(plain_text_output(path))
    return go


def fn_pdfminer(path):
    from pdfminer.high_level import extract_text
    def go():
        return len(extract_text(path))
    return go


def fn_pdfplumber(path):
    import pdfplumber
    def go():
        n = 0
        with pdfplumber.open(path) as pdf:
            for page in pdf.pages:
                t = page.extract_text() or ""
                n += len(t)
        return n
    return go


def fn_markitdown(path):
    from markitdown import MarkItDown
    md = MarkItDown()
    def go():
        r = md.convert(path)
        return len(r.text_content or "")
    return go


def fn_pypdf(path):
    from pypdf import PdfReader
    def go():
        reader = PdfReader(path)
        n = 0
        for page in reader.pages:
            t = page.extract_text() or ""
            n += len(t)
        return n
    return go


LIBS = [
    ("flashpdf-MT",   lambda p: fn_flashpdf(p, parallel=True), ITERS),
    ("flashpdf-ST",   lambda p: fn_flashpdf(p, parallel=False), ITERS),
    ("pdf_oxide",     fn_pdf_oxide, ITERS),
    ("PyMuPDF",       fn_pymupdf, ITERS),
    ("pypdfium2",     fn_pypdfium2, ITERS),
    ("pypdf",         fn_pypdf, ITERS),
    ("pdfminer",      fn_pdfminer, ITERS),
    ("pdfplumber",    fn_pdfplumber, 10),
    ("pdftext",       fn_pdftext, 5),
    ("pymupdf4llm",   fn_pymupdf4llm, 5),
    ("markitdown",    fn_markitdown, 3),
]


def main():
    print(f"Iters per PDF: fast libs={ITERS}, slow libs=3-10. 1 warm-up, text-only.\n")
    header = f"{'PDF':<14} {'Library':<14} {'Mean':>10} {'p99':>10} {'Chars':>10}"
    print(header)
    print("-" * len(header))
    for path, label in PDFS:
        for lib, factory, iters in LIBS:
            try:
                r = run(factory(path), iters=iters)
                print(f"{label:<14} {lib:<14} {r['mean_ms']:>8.2f}ms {r['p99_ms']:>8.2f}ms {r['chars']:>10}", flush=True)
            except Exception as e:
                print(f"{label:<14} {lib:<14} {'ERROR':>10} {'-':>10} {str(e)[:40]}", flush=True)
        print()


if __name__ == "__main__":
    main()
