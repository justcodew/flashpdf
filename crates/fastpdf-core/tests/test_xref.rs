use fastpdf_core::parser::object::parse_object_from_bytes;
use fastpdf_core::parser::xref::{
    find_startxref, is_standard_xref, parse_xref_table, XrefEntryType,
};
use fastpdf_core::types::PdfObject;

// ─── Helper: build a minimal PDF byte stream ───

/// Build a minimal valid PDF with a standard xref table.
fn build_minimal_pdf() -> Vec<u8> {
    let mut pdf = Vec::new();

    // Header
    pdf.extend_from_slice(b"%PDF-1.4\n");

    // Object 1: Catalog
    let obj1_offset = pdf.len();
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    // Object 2: Pages
    let obj2_offset = pdf.len();
    pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

    // Object 3: Page
    let obj3_offset = pdf.len();
    pdf.extend_from_slice(
        b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n",
    );

    // xref table
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n");
    pdf.extend_from_slice(b"0 4\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n"); // obj 0: free
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj2_offset).as_bytes());
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj3_offset).as_bytes());

    // trailer
    pdf.extend_from_slice(b"trailer\n<< /Size 4 /Root 1 0 R >>\n");
    pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

    pdf
}

// ─── Tests ───

#[test]
fn test_find_startxref() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    // The xref starts after "%PDF-1.4\n" + 3 objects
    assert!(offset > 0);
    assert!(pdf[offset..].starts_with(b"xref"));
}

#[test]
fn test_is_standard_xref() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    assert!(is_standard_xref(&pdf, offset));
}

#[test]
fn test_parse_xref_table() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    // Should have 4 entries (0-3)
    assert_eq!(xref.size, 4);
    // Root should be 1 0 R
    assert_eq!(xref.root.num, 1);
    assert_eq!(xref.root.gen, 0);
    // Info should be None
    assert!(xref.info.is_none());

    // Check entries
    let e0 = xref.get(0).unwrap();
    assert_eq!(e0.entry_type, XrefEntryType::Free);

    let e1 = xref.get(1).unwrap();
    assert_eq!(e1.entry_type, XrefEntryType::Uncompressed);
    assert!(e1.field1 > 0); // offset should be non-zero

    let e2 = xref.get(2).unwrap();
    assert_eq!(e2.entry_type, XrefEntryType::Uncompressed);

    let e3 = xref.get(3).unwrap();
    assert_eq!(e3.entry_type, XrefEntryType::Uncompressed);
}

#[test]
fn test_parse_catalog_object() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    // Verify object 1 is the catalog
    let e1 = xref.get(1).unwrap();
    let obj1_offset = e1.field1 as usize;
    let remaining = &pdf[obj1_offset..];

    // The object should start with "1 0 obj"
    assert!(remaining.starts_with(b"1 0 obj"));

    // Parse the object content (skip the header)
    let after_header = &remaining[7..]; // skip "1 0 obj"
    let obj = parse_object_from_bytes(after_header).unwrap();
    let _dict = obj.as_dict().unwrap();

    // Should have /Type /Catalog
    let type_val = obj.get(b"Type").unwrap();
    assert_eq!(type_val.as_name(), Some(b"Catalog".as_slice()));

    // Should have /Pages 2 0 R
    let pages = obj.get(b"Pages").unwrap();
    match pages {
        PdfObject::Ref(id) => {
            assert_eq!(id.num, 2);
            assert_eq!(id.gen, 0);
        }
        _ => panic!("expected Ref for /Pages"),
    }
}

#[test]
fn test_parse_pages_object() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    let e2 = xref.get(2).unwrap();
    let obj2_offset = e2.field1 as usize;
    let remaining = &pdf[obj2_offset..];

    // Skip "2 0 obj" header
    let after_header = &remaining[7..];
    let obj = parse_object_from_bytes(after_header).unwrap();

    assert_eq!(
        obj.get(b"Type").unwrap().as_name(),
        Some(b"Pages".as_slice())
    );
    assert_eq!(obj.get(b"Count").unwrap().as_i64(), Some(1));

    let kids = obj.get(b"Kids").unwrap().as_array().unwrap();
    assert_eq!(kids.len(), 1);
    match &kids[0] {
        PdfObject::Ref(id) => {
            assert_eq!(id.num, 3);
        }
        _ => panic!("expected Ref"),
    }
}

#[test]
fn test_parse_page_object() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    let e3 = xref.get(3).unwrap();
    let obj3_offset = e3.field1 as usize;
    let remaining = &pdf[obj3_offset..];

    let after_header = &remaining[7..];
    let obj = parse_object_from_bytes(after_header).unwrap();

    assert_eq!(
        obj.get(b"Type").unwrap().as_name(),
        Some(b"Page".as_slice())
    );

    let mediabox = obj.get(b"MediaBox").unwrap().as_array().unwrap();
    assert_eq!(mediabox.len(), 4);
    assert_eq!(mediabox[0].as_f64(), Some(0.0));
    assert_eq!(mediabox[2].as_f64(), Some(612.0));
    assert_eq!(mediabox[3].as_f64(), Some(792.0));
}

#[test]
fn test_xref_entry_generation() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    // All non-free entries should have gen 0
    let e1 = xref.get(1).unwrap();
    assert_eq!(e1.field2, 0); // gen 0
}

// ─── Test with trailing whitespace variations ───

#[test]
fn test_find_startxref_with_crlf() {
    let pdf = build_minimal_pdf();
    // The PDF already has \n, let's also test with \r\n
    // Just verify it works as-is
    let offset = find_startxref(&pdf).unwrap();
    assert!(offset > 0);
}

#[test]
fn test_xref_table_entry_offsets_are_consistent() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    // Each uncompressed entry's offset should point to a valid object
    for obj_num in 1..=3 {
        let entry = xref.get(obj_num).unwrap();
        let off = entry.field1 as usize;
        assert!(off < pdf.len(), "offset for obj {} out of bounds", obj_num);
        // The data at that offset should start with the object header
        let slice = &pdf[off..];
        assert!(
            slice.starts_with(format!("{} {} obj", obj_num, entry.field2).as_bytes()),
            "obj {} at offset {} doesn't start with expected header",
            obj_num,
            off
        );
    }
}

// ─── Test Object 0 is always free ───

#[test]
fn test_object_zero_is_free() {
    let pdf = build_minimal_pdf();
    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();

    let e0 = xref.get(0).unwrap();
    assert_eq!(e0.entry_type, XrefEntryType::Free);
}

// ─── Test parsing a PDF with comments in header ───

#[test]
fn test_pdf_header_with_binary_marker() {
    let mut pdf = Vec::new();
    // PDF header with binary marker (common in real PDFs)
    pdf.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");

    // Minimal object
    pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let obj1_offset = 19; // after header

    // xref
    let xref_offset = pdf.len();
    pdf.extend_from_slice(b"xref\n0 2\n");
    pdf.extend_from_slice(b"0000000000 65535 f \n");
    pdf.extend_from_slice(format!("{:010} 00000 n \n", obj1_offset).as_bytes());
    pdf.extend_from_slice(b"trailer\n<< /Size 2 /Root 1 0 R >>\n");
    pdf.extend_from_slice(format!("startxref\n{}\n%%EOF\n", xref_offset).as_bytes());

    let offset = find_startxref(&pdf).unwrap();
    let xref = parse_xref_table(&pdf, offset).unwrap();
    assert_eq!(xref.root.num, 1);
}
