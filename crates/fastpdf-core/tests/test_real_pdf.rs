use fastpdf_core::{extract, ExtractOptions};
use std::time::Instant;

#[test]
fn test_extract_real_pdf() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test_data")
        .join("2604.11578v1.pdf");
    let path = path.to_str().unwrap();

    // Warm up: open and parse xref
    let options = ExtractOptions {
        page_parallel: false,
        include_images: false,
        batch_size: 0,
        ..Default::default()
    };

    let start = Instant::now();
    let result = extract(path, &options).expect("Failed to extract PDF");
    let elapsed = start.elapsed();

    let total_chars: usize = result
        .pages
        .iter()
        .map(|p| {
            p.blocks
                .iter()
                .map(|b| {
                    b.lines
                        .iter()
                        .map(|l| l.spans.iter().map(|s| s.text.len()).sum::<usize>())
                        .sum::<usize>()
                })
                .sum::<usize>()
        })
        .sum();

    let total_blocks: usize = result.pages.iter().map(|p| p.blocks.len()).sum();
    let total_lines: usize = result
        .pages
        .iter()
        .map(|p| p.blocks.iter().map(|b| b.lines.len()).sum::<usize>())
        .sum();
    let total_spans: usize = result
        .pages
        .iter()
        .map(|p| {
            p.blocks
                .iter()
                .map(|b| b.lines.iter().map(|l| l.spans.len()).sum::<usize>())
                .sum::<usize>()
        })
        .sum();

    println!("=== fastpdf extraction results ===");
    println!("Pages:        {}", result.pages.len());
    println!("Blocks:       {}", total_blocks);
    println!("Lines:        {}", total_lines);
    println!("Spans:        {}", total_spans);
    println!("Total chars:  {}", total_chars);
    println!("Time:         {:.2?}", elapsed);
    println!(
        "Speed:        {:.0} pages/sec",
        result.pages.len() as f64 / elapsed.as_secs_f64()
    );

    // Print first few spans as sample
    println!("\n=== Sample text (first 5 spans) ===");
    let mut count = 0;
    for page in &result.pages {
        for block in &page.blocks {
            for line in &block.lines {
                for span in &line.spans {
                    if count < 5 && !span.text.trim().is_empty() {
                        println!("  [{} {:.0}pt] {}", span.font, span.size, span.text);
                        count += 1;
                    }
                }
            }
        }
    }

    // Basic sanity checks
    assert!(result.pages.len() > 0, "Should have at least 1 page");
    assert!(total_chars > 0, "Should have extracted some text");
}

#[test]
fn test_extract_real_pdf_parallel() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test_data")
        .join("2604.11578v1.pdf");
    let path = path.to_str().unwrap();

    let options = ExtractOptions {
        page_parallel: true,
        include_images: false,
        batch_size: 50,
        ..Default::default()
    };

    let start = Instant::now();
    let result = extract(path, &options).expect("Failed to extract PDF");
    let elapsed = start.elapsed();

    println!("\n=== fastpdf parallel extraction ===");
    println!("Pages: {}", result.pages.len());
    println!("Time:  {:.2?}", elapsed);
    println!(
        "Speed: {:.0} pages/sec",
        result.pages.len() as f64 / elapsed.as_secs_f64()
    );

    assert!(result.pages.len() > 0);
}

#[test]
fn test_extract_real_pdf_with_images() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("test_data")
        .join("2604.11578v1.pdf");
    let path = path.to_str().unwrap();

    let options = ExtractOptions {
        page_parallel: false,
        include_images: true,
        batch_size: 0,
        ..Default::default()
    };

    let start = Instant::now();
    let result = extract(path, &options).expect("Failed to extract PDF");
    let elapsed = start.elapsed();

    let total_images: usize = result.pages.iter().map(|p| p.images.len()).sum();

    println!("\n=== fastpdf with images ===");
    println!("Pages:  {}", result.pages.len());
    println!("Images: {}", total_images);
    println!("Time:   {:.2?}", elapsed);
}
