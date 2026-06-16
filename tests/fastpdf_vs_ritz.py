"""
Compare text extraction speed: fastpdf vs ritz vs PyMuPDF
"""
import time
import sys
import statistics

PDF_PATH = "/Users/xiongzhaolong/Downloads/claude_pro/fastpdf/test_data/2604.11578v1.pdf"
ITERS = 5
WARMUP = 1


def bench_pymupdf(pdf_path, iters):
    import fitz
    times = []
    for _ in range(iters):
        start = time.perf_counter()
        doc = fitz.open(pdf_path)
        for page in doc:
            page.get_text("dict")
        doc.close()
        times.append(time.perf_counter() - start)
    return times


def bench_ritz(pdf_path, iters):
    sys.path.insert(0, "/Users/xiongzhaolong/Downloads/claude-pro/202604-job/pdf_pro/ritz/python")
    import ritz
    times = []
    for _ in range(iters):
        start = time.perf_counter()
        doc = ritz.open(pdf_path)
        for i in range(doc.page_count):
            page = doc.load_page(i)
            page.get_text("dict")
        del doc
        times.append(time.perf_counter() - start)
    return times


def bench_fastpdf(pdf_path, iters):
    sys.path.insert(0, "/Users/xiongzhaolong/Downloads/claude_pro/fastpdf/python")
    import fastpdf
    times = []
    for _ in range(iters):
        start = time.perf_counter()
        blocks, images = fastpdf.extract(pdf_path, include_images=False)
        times.append(time.perf_counter() - start)
    return times


def main():
    pdf_path = PDF_PATH
    if len(sys.argv) > 1:
        pdf_path = sys.argv[1]

    print(f"PDF: {pdf_path}")
    print(f"Iterations: {ITERS} (warmup: {WARMUP})")
    print("=" * 60)

    # Warmup
    for _ in range(WARMUP):
        bench_pymupdf(pdf_path, 1)
        bench_ritz(pdf_path, 1)
        bench_fastpdf(pdf_path, 1)

    # Benchmark
    pymupdf_times = bench_pymupdf(pdf_path, ITERS)
    ritz_times = bench_ritz(pdf_path, ITERS)
    fastpdf_times = bench_fastpdf(pdf_path, ITERS)

    pymupdf_avg = statistics.mean(pymupdf_times)
    ritz_avg = statistics.mean(ritz_times)
    fastpdf_avg = statistics.mean(fastpdf_times)

    pymupdf_std = statistics.stdev(pymupdf_times) if len(pymupdf_times) > 1 else 0
    ritz_std = statistics.stdev(ritz_times) if len(ritz_times) > 1 else 0
    fastpdf_std = statistics.stdev(fastpdf_times) if len(fastpdf_times) > 1 else 0

    print(f"\n{'Engine':<12} {'Avg (ms)':>10} {'Std (ms)':>10} {'Speedup':>10}")
    print("-" * 45)
    print(f"{'PyMuPDF':<12} {pymupdf_avg*1000:>10.2f} {pymupdf_std*1000:>10.2f} {'1.00x':>10}")
    print(f"{'ritz':<12} {ritz_avg*1000:>10.2f} {ritz_std*1000:>10.2f} {pymupdf_avg/ritz_avg:>9.2f}x")
    print(f"{'fastpdf':<12} {fastpdf_avg*1000:>10.2f} {fastpdf_std*1000:>10.2f} {pymupdf_avg/fastpdf_avg:>9.2f}x")

    print(f"\n{'fastpdf vs ritz:':<20} {ritz_avg/fastpdf_avg:.2f}x")


if __name__ == "__main__":
    main()
