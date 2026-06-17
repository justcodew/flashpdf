use criterion::{black_box, criterion_group, criterion_main, Criterion};
use fastpdf_core::parser::content_stream::scan_content_stream;
use fastpdf_core::ExtractOptions;

fn bench_content_stream_scan(c: &mut Criterion) {
    // Simple text content stream
    let simple = b"BT /F1 12 Tf 100 700 Td (Hello World this is a test) Tj ET";

    // Complex content with multiple blocks and kerning
    let complex = b"BT /F1 12 Tf 100 700 Td [(Hello) -100 (World)] TJ ET BT /F2 10 Tf 50 650 Td (Second line of text) Tj ET BT /F1 8 Tf 200 600 Td <48656C6C6F> Tj ET";

    // Large content with many operators
    let mut large = Vec::new();
    large.extend_from_slice(b"BT /F1 12 Tf ");
    for i in 0..100 {
        large.extend_from_slice(
            format!("100 {} Td (Line {} text content here) Tj ", 700 - i * 14, i).as_bytes(),
        );
    }
    large.extend_from_slice(b"ET");

    c.bench_function("content_stream_simple", |b| {
        b.iter(|| scan_content_stream(black_box(simple)))
    });

    c.bench_function("content_stream_complex", |b| {
        b.iter(|| scan_content_stream(black_box(complex)))
    });

    c.bench_function("content_stream_large", |b| {
        b.iter(|| scan_content_stream(black_box(&large)))
    });
}

fn bench_full_extraction(c: &mut Criterion) {
    let test_pdf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test_data")
        .join("2604.11578v1.pdf");

    if test_pdf.exists() {
        let path_str = test_pdf.to_str().unwrap();
        let options = ExtractOptions {
            page_parallel: false,
            file_parallel: false,
            include_images: false,
            gpu: false,
            batch_size: 0,
        };

        c.bench_function("full_extract_single_page", |b| {
            b.iter(|| {
                let result = fastpdf_core::extract(black_box(path_str), &options);
                black_box(result).ok();
            })
        });

        c.bench_function("full_extract_with_images", |b| {
            let opts = ExtractOptions {
                include_images: true,
                ..options.clone()
            };
            b.iter(|| {
                let result = fastpdf_core::extract(black_box(path_str), &opts);
                black_box(result).ok();
            })
        });

        c.bench_function("full_extract_parallel", |b| {
            let opts = ExtractOptions {
                page_parallel: true,
                ..options.clone()
            };
            b.iter(|| {
                let result = fastpdf_core::extract(black_box(path_str), &opts);
                black_box(result).ok();
            })
        });
    }
}

fn bench_parser(c: &mut Criterion) {
    // Object parser benchmark
    let dict_data = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> >>";

    c.bench_function("parse_object_dict", |b| {
        b.iter(|| {
            let mut cur = fastpdf_core::Cursor::new(black_box(dict_data));
            fastpdf_core::parse_object(&mut cur)
        })
    });

    let array_data = b"[1 2 3 4.5 -6 (hello) /Name 7 0 R]";

    c.bench_function("parse_object_array", |b| {
        b.iter(|| {
            let mut cur = fastpdf_core::Cursor::new(black_box(array_data));
            fastpdf_core::parse_object(&mut cur)
        })
    });
}

criterion_group!(
    benches,
    bench_content_stream_scan,
    bench_full_extraction,
    bench_parser
);
criterion_main!(benches);
