use flashpdf_core::parse_object_from_bytes;
use flashpdf_core::types::PdfObject;

// ─── Integer tests ───

#[test]
fn test_integer_positive() {
    let obj = parse_object_from_bytes(b"42").unwrap();
    assert_eq!(obj.as_i64(), Some(42));
}

#[test]
fn test_integer_negative() {
    let obj = parse_object_from_bytes(b"-17").unwrap();
    assert_eq!(obj.as_i64(), Some(-17));
}

#[test]
fn test_integer_positive_sign() {
    let obj = parse_object_from_bytes(b"+5").unwrap();
    assert_eq!(obj.as_i64(), Some(5));
}

#[test]
fn test_integer_zero() {
    let obj = parse_object_from_bytes(b"0").unwrap();
    assert_eq!(obj.as_i64(), Some(0));
}

#[test]
fn test_integer_with_trailing_ws() {
    let obj = parse_object_from_bytes(b"123  ").unwrap();
    assert_eq!(obj.as_i64(), Some(123));
}

// ─── Real number tests ───

#[test]
fn test_real_simple() {
    let obj = parse_object_from_bytes(b"3.5").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - 3.5).abs() < 1e-10);
}

#[test]
fn test_real_negative() {
    let obj = parse_object_from_bytes(b"-2.5").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - (-2.5)).abs() < 1e-10);
}

#[test]
fn test_real_no_integer_part() {
    let obj = parse_object_from_bytes(b".5").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - 0.5).abs() < 1e-10);
}

#[test]
fn test_real_exponent() {
    let obj = parse_object_from_bytes(b"1.5e3").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - 1500.0).abs() < 1e-10);
}

#[test]
fn test_real_negative_exponent() {
    let obj = parse_object_from_bytes(b"5E-2").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - 0.05).abs() < 1e-10);
}

// ─── Boolean tests ───

#[test]
fn test_bool_true() {
    let obj = parse_object_from_bytes(b"true").unwrap();
    match obj {
        PdfObject::Bool(true) => {}
        _ => panic!("expected Bool(true), got {:?}", obj),
    }
}

#[test]
fn test_bool_false() {
    let obj = parse_object_from_bytes(b"false").unwrap();
    match obj {
        PdfObject::Bool(false) => {}
        _ => panic!("expected Bool(false), got {:?}", obj),
    }
}

#[test]
fn test_bool_with_delimiter_after() {
    // "true]" — true followed by ] should parse correctly
    let obj = parse_object_from_bytes(b"true]").unwrap();
    match obj {
        PdfObject::Bool(true) => {}
        _ => panic!("expected Bool(true), got {:?}", obj),
    }
}

// ─── Null test ───

#[test]
fn test_null() {
    let obj = parse_object_from_bytes(b"null").unwrap();
    assert!(obj.is_null());
}

// ─── Name tests ───

#[test]
fn test_name_simple() {
    let obj = parse_object_from_bytes(b"/Type").unwrap();
    assert_eq!(obj.as_name(), Some(b"Type".as_slice()));
}

#[test]
fn test_name_with_hex_escape() {
    let obj = parse_object_from_bytes(b"/A#42C").unwrap();
    // #42 = 'B'
    assert_eq!(obj.as_name(), Some(b"ABC".as_slice()));
}

#[test]
fn test_name_slash_only() {
    let obj = parse_object_from_bytes(b"/").unwrap();
    assert_eq!(obj.as_name(), Some(b"".as_slice()));
}

#[test]
fn test_name_with_special_chars() {
    let obj = parse_object_from_bytes(b"/Helvetica-Bold").unwrap();
    assert_eq!(obj.as_name(), Some(b"Helvetica-Bold".as_slice()));
}

// ─── String literal tests ───

#[test]
fn test_string_simple() {
    let obj = parse_object_from_bytes(b"(Hello, World!)").unwrap();
    assert_eq!(obj.as_str(), Some(b"Hello, World!".as_slice()));
}

#[test]
fn test_string_empty() {
    let obj = parse_object_from_bytes(b"()").unwrap();
    assert_eq!(obj.as_str(), Some(b"".as_slice()));
}

#[test]
fn test_string_with_escape() {
    let obj = parse_object_from_bytes(b"(Hello\\nWorld)").unwrap();
    // Raw bytes include the \n as two chars '\' and 'n'
    // The parser just skips the escaped byte, so we get "Hello" + skip 'n' + "World"
    // Actually, the parser skips the next byte after \, so it skips 'n'
    // The raw content between ( and ) is: Hello\nWorld
    // The parser returns everything between ( and ) without processing escapes
    assert_eq!(obj.as_str(), Some(b"Hello\\nWorld".as_slice()));
}

#[test]
fn test_string_nested_parens() {
    let obj = parse_object_from_bytes(b"(a(b)c)").unwrap();
    assert_eq!(obj.as_str(), Some(b"a(b)c".as_slice()));
}

#[test]
fn test_string_with_escaped_parens() {
    let obj = parse_object_from_bytes(b"(a\\(b\\)c)").unwrap();
    // The parser skips the byte after \, so \\( becomes just (
    // Actually, the raw bytes between ( and ) are: a\(b\)c
    // When we hit \, we consume the next byte (which is ( or )), so we skip it
    // The result includes everything between outer parens
    assert_eq!(obj.as_str(), Some(b"a\\(b\\)c".as_slice()));
}

// ─── Hex string tests ───

#[test]
fn test_hex_string_simple() {
    let obj = parse_object_from_bytes(b"<48656C6C6F>").unwrap();
    assert_eq!(obj.as_str(), Some(b"48656C6C6F".as_slice()));
}

#[test]
fn test_hex_string_with_spaces() {
    let obj = parse_object_from_bytes(b"<48 65 6C 6C 6F>").unwrap();
    assert_eq!(obj.as_str(), Some(b"48 65 6C 6C 6F".as_slice()));
}

#[test]
fn test_hex_string_empty() {
    let obj = parse_object_from_bytes(b"<>").unwrap();
    assert_eq!(obj.as_str(), Some(b"".as_slice()));
}

// ─── Array tests ───

#[test]
fn test_array_simple() {
    let obj = parse_object_from_bytes(b"[1 2 3]").unwrap();
    let arr = obj.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0].as_i64(), Some(1));
    assert_eq!(arr[1].as_i64(), Some(2));
    assert_eq!(arr[2].as_i64(), Some(3));
}

#[test]
fn test_array_empty() {
    let obj = parse_object_from_bytes(b"[]").unwrap();
    let arr = obj.as_array().unwrap();
    assert_eq!(arr.len(), 0);
}

#[test]
fn test_array_mixed_types() {
    let obj = parse_object_from_bytes(b"[1 /Name (hello) true null]").unwrap();
    let arr = obj.as_array().unwrap();
    assert_eq!(arr.len(), 5);
    assert_eq!(arr[0].as_i64(), Some(1));
    assert_eq!(arr[1].as_name(), Some(b"Name".as_slice()));
    assert_eq!(arr[2].as_str(), Some(b"hello".as_slice()));
    match arr[3] {
        PdfObject::Bool(true) => {}
        _ => panic!("expected Bool(true)"),
    }
    assert!(arr[4].is_null());
}

#[test]
fn test_array_nested() {
    let obj = parse_object_from_bytes(b"[[1 2] [3 4]]").unwrap();
    let arr = obj.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    let inner1 = arr[0].as_array().unwrap();
    assert_eq!(inner1[0].as_i64(), Some(1));
    assert_eq!(inner1[1].as_i64(), Some(2));
}

// ─── Dictionary tests ───

#[test]
fn test_dict_simple() {
    let obj = parse_object_from_bytes(b"<< /Type /Page /Count 1 >>").unwrap();
    let dict = obj.as_dict().unwrap();
    assert_eq!(dict.len(), 2);
    assert_eq!(dict[0].0, b"Type");
    assert_eq!(dict[0].1.as_name(), Some(b"Page".as_slice()));
    assert_eq!(dict[1].0, b"Count");
    assert_eq!(dict[1].1.as_i64(), Some(1));
}

#[test]
fn test_dict_empty() {
    let obj = parse_object_from_bytes(b"<< >>").unwrap();
    let dict = obj.as_dict().unwrap();
    assert_eq!(dict.len(), 0);
}

#[test]
fn test_dict_nested() {
    let obj = parse_object_from_bytes(b"<< /Parent << /Type /Pages >> >>").unwrap();
    let dict = obj.as_dict().unwrap();
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, b"Parent");
    let inner = dict[0].1.as_dict().unwrap();
    assert_eq!(inner[0].0, b"Type");
}

#[test]
fn test_dict_get() {
    let obj = parse_object_from_bytes(b"<< /Type /Page /Count 5 >>").unwrap();
    assert_eq!(
        obj.get(b"Type").unwrap().as_name(),
        Some(b"Page".as_slice())
    );
    assert_eq!(obj.get(b"Count").unwrap().as_i64(), Some(5));
    assert!(obj.get(b"Missing").is_none());
}

// ─── Reference tests ───

#[test]
fn test_ref_simple() {
    let obj = parse_object_from_bytes(b"10 0 R").unwrap();
    match obj {
        PdfObject::Ref(id) => {
            assert_eq!(id.num, 10);
            assert_eq!(id.gen, 0);
        }
        _ => panic!("expected Ref, got {:?}", obj),
    }
}

#[test]
fn test_ref_gen_nonzero() {
    let obj = parse_object_from_bytes(b"5 3 R").unwrap();
    match obj {
        PdfObject::Ref(id) => {
            assert_eq!(id.num, 5);
            assert_eq!(id.gen, 3);
        }
        _ => panic!("expected Ref, got {:?}", obj),
    }
}

// ─── Whitespace and comment handling ───

#[test]
fn test_leading_whitespace() {
    let obj = parse_object_from_bytes(b"   42").unwrap();
    assert_eq!(obj.as_i64(), Some(42));
}

#[test]
fn test_comment_before_object() {
    let obj = parse_object_from_bytes(b"% this is a comment\n42").unwrap();
    assert_eq!(obj.as_i64(), Some(42));
}

#[test]
fn test_multiple_comments() {
    let obj = parse_object_from_bytes(b"% comment 1\n% comment 2\n42").unwrap();
    assert_eq!(obj.as_i64(), Some(42));
}

// ─── Stream test ───

#[test]
fn test_dict_followed_by_stream() {
    // A typical stream: dict with /Length, then stream keyword
    let data = b"<< /Length 11 >>\nstream\nHello World\nendstream";
    let obj = parse_object_from_bytes(data).unwrap();
    // parse_object returns the dict; stream parsing is separate
    let dict = obj.as_dict().unwrap();
    assert_eq!(dict.len(), 1);
    assert_eq!(dict[0].0, b"Length");
    assert_eq!(dict[0].1.as_i64(), Some(11));
}

// ─── Complex nested structure ───

#[test]
fn test_complex_structure() {
    let data = b"<<\
        /Type /Page\
        /Parent 3 0 R\
        /MediaBox [0 0 612 792]\
        /Contents 5 0 R\
        /Resources <<\
            /Font << /F1 7 0 R >>\
        >>\
    >>";
    let obj = parse_object_from_bytes(data).unwrap();
    assert_eq!(
        obj.get(b"Type").unwrap().as_name(),
        Some(b"Page".as_slice())
    );
    assert_eq!(obj.get(b"Parent").unwrap().as_ref().unwrap().num, 3);
    let mediabox = obj.get(b"MediaBox").unwrap().as_array().unwrap();
    assert_eq!(mediabox.len(), 4);
    assert_eq!(mediabox[2].as_f64(), Some(612.0));
}

// ─── Type name checks ───

#[test]
fn test_type_names() {
    assert_eq!(
        parse_object_from_bytes(b"42").unwrap().type_name(),
        "integer"
    );
    assert_eq!(
        parse_object_from_bytes(b"3.14").unwrap().type_name(),
        "real"
    );
    assert_eq!(
        parse_object_from_bytes(b"true").unwrap().type_name(),
        "boolean"
    );
    assert_eq!(
        parse_object_from_bytes(b"null").unwrap().type_name(),
        "null"
    );
    assert_eq!(
        parse_object_from_bytes(b"/Name").unwrap().type_name(),
        "name"
    );
    assert_eq!(
        parse_object_from_bytes(b"(str)").unwrap().type_name(),
        "string"
    );
    assert_eq!(
        parse_object_from_bytes(b"<AB>").unwrap().type_name(),
        "hexstring"
    );
    assert_eq!(
        parse_object_from_bytes(b"[1]").unwrap().type_name(),
        "array"
    );
    assert_eq!(
        parse_object_from_bytes(b"<< >>").unwrap().type_name(),
        "dictionary"
    );
    assert_eq!(
        parse_object_from_bytes(b"1 0 R").unwrap().type_name(),
        "reference"
    );
}

// ─── Edge cases ───

#[test]
fn test_integer_boundary_u32() {
    // Max u32
    let obj = parse_object_from_bytes(b"4294967295").unwrap();
    assert_eq!(obj.as_i64(), Some(4294967295));
}

#[test]
fn test_real_very_small() {
    let obj = parse_object_from_bytes(b"0.001").unwrap();
    let f = obj.as_f64().unwrap();
    assert!((f - 0.001).abs() < 1e-15);
}

#[test]
fn test_name_hash_at_end() {
    // A name ending with # but not enough hex digits should just be treated as regular
    let obj = parse_object_from_bytes(b"/Test#").unwrap();
    assert_eq!(obj.as_name(), Some(b"Test#".as_slice()));
}
