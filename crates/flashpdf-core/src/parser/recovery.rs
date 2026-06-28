/// Fallback xref recovery via memchr full-file scan.
/// Used when the standard xref table is corrupted or missing.
use crate::parser::object::{ParseError, ParseResult};
use crate::parser::xref::{XrefEntry, XrefTable};
use crate::types::ObjectId;
use std::collections::HashMap;

/// Scan the entire file for `N G obj` patterns and build an xref table.
/// This is the fallback when the standard xref is corrupt.
pub fn recover_xref_by_scan(data: &[u8]) -> ParseResult<XrefTable> {
    let mut entries: HashMap<u32, XrefEntry> = HashMap::new();
    let mut root: Option<ObjectId> = None;

    // Use memchr to find all "obj" occurrences
    let needle = b"obj";
    let mut offset = 0;

    while offset < data.len() {
        let remaining = &data[offset..];
        let Some(pos) = memchr::memmem::find(remaining, needle) else {
            break;
        };

        let abs_pos = offset + pos;

        // Check if this is actually `N G obj` (not inside a string/stream)
        if let Some((obj_num, gen, header_start)) = try_parse_obj_header(data, abs_pos) {
            // If it's the catalog, record the root
            if root.is_none() {
                if let Some(obj) = try_parse_object_at(data, abs_pos) {
                    if let Some(PdfObj::Dict(d)) = Some(&obj) {
                        if is_catalog(d) {
                            let _root_ref = find_ref_in_dict(d, b"Root").or({
                                // The catalog IS object 1 typically, and /Pages is its child
                                // For recovery, we'll just record the first catalog-like object
                                None
                            });
                            if let Some(_r) = find_ref_in_dict(d, b"Pages") {
                                // This is actually the catalog; Root is itself
                                root = Some(ObjectId::new(obj_num, gen));
                            }
                        }
                    }
                }
            }

            entries.insert(obj_num, XrefEntry::uncompressed(header_start as u32, gen));
        }

        offset = abs_pos + needle.len();
    }

    // If we found entries, try to find the root
    if root.is_none() {
        // Look for catalog-like objects
        for (&obj_num, entry) in &entries {
            let off = entry.field1 as usize;
            if let Some(obj) = try_parse_object_at(data, off) {
                if let PdfObj::Dict(d) = &obj {
                    if d.iter().any(|(k, v)| {
                        *k == b"Type" && matches!(v, PdfObj::Name(n) if n == b"Catalog")
                    }) {
                        root = Some(ObjectId::new(obj_num, entry.field2));
                        break;
                    }
                }
            }
        }
    }

    let root = root.ok_or(ParseError::Message(
        "recovery: could not find Catalog root".to_string(),
    ))?;

    let size = entries.keys().max().map_or(0, |&k| k + 1);

    Ok(XrefTable {
        entries,
        root,
        size,
        info: None,
        encrypt: None,
        encrypt_present: false,
        trailer_offset: None,
        id_first: None,
        prev_offset: None,
    })
}

/// Try to parse `N G obj` at the given position.
/// Returns (obj_num, gen, header_start) where header_start is the byte offset
/// of the leading digit of the object number — the offset that parse_object_at
/// expects. Recording obj_keyword_pos instead would leave parse_object_at
/// reading "obj\n<<..." which has no leading digit → InvalidNumber on every
/// recovery-built xref entry.
fn try_parse_obj_header(data: &[u8], obj_keyword_pos: usize) -> Option<(u32, u16, usize)> {
    // The "obj" keyword should be preceded by "N G " (with optional whitespace)
    // Scan backwards from obj_keyword_pos
    if obj_keyword_pos < 4 {
        return None;
    }

    // Find the last whitespace before "obj"
    let mut pos = obj_keyword_pos;
    // Skip any whitespace right before "obj"
    while pos > 0 && (data[pos - 1] == b' ' || data[pos - 1] == b'\t') {
        pos -= 1;
    }
    if pos == 0 {
        return None;
    }

    // Now pos points just after the generation number
    let gen_end = pos;

    // Scan backwards for the generation number
    while pos > 0 && data[pos - 1].is_ascii_digit() {
        pos -= 1;
    }
    let gen_start = pos;
    if gen_start == gen_end {
        return None;
    }

    // Skip whitespace
    while pos > 0 && (data[pos - 1] == b' ' || data[pos - 1] == b'\t') {
        pos -= 1;
    }

    // Scan backwards for the object number
    let num_end = pos;
    while pos > 0 && data[pos - 1].is_ascii_digit() {
        pos -= 1;
    }
    let num_start = pos;
    if num_start == num_end {
        return None;
    }

    // Check that we're at the start of a line (or preceded by whitespace/newline)
    if pos > 0 && !matches!(data[pos - 1], b'\n' | b'\r' | b' ' | b'\t') {
        return None;
    }

    let num_str = std::str::from_utf8(&data[num_start..num_end]).ok()?;
    let gen_str = std::str::from_utf8(&data[gen_start..gen_end]).ok()?;

    let obj_num: u32 = num_str.parse().ok()?;
    let gen: u16 = gen_str.parse().ok()?;

    Some((obj_num, gen, num_start))
}

/// Minimal object representation for recovery scanning
enum PdfObj<'a> {
    Dict(Vec<(&'a [u8], PdfObj<'a>)>),
    Name(&'a [u8]),
    Ref(u32, u16),
    _Other,
}

/// Try to parse just enough of an object to identify it (catalog, etc.)
fn try_parse_object_at(data: &[u8], obj_keyword_pos: usize) -> Option<PdfObj<'_>> {
    // Skip "obj" keyword
    let after_obj = obj_keyword_pos + 3;
    if after_obj >= data.len() {
        return None;
    }

    // Skip whitespace
    let mut pos = after_obj;
    while pos < data.len() && matches!(data[pos], b' ' | b'\t' | b'\n' | b'\r') {
        pos += 1;
    }

    if pos >= data.len() {
        return None;
    }

    // Only parse dicts (for catalog detection)
    if data[pos] == b'<' && pos + 1 < data.len() && data[pos + 1] == b'<' {
        parse_minimal_dict(&data[pos..])
    } else {
        Some(PdfObj::_Other)
    }
}

/// Parse a minimal dict for recovery purposes (just names and refs)
fn parse_minimal_dict(data: &[u8]) -> Option<PdfObj<'_>> {
    if data.len() < 4 || data[0] != b'<' || data[1] != b'<' {
        return None;
    }

    let mut entries = Vec::new();
    let mut pos = 2;

    loop {
        // Skip whitespace
        while pos < data.len() && matches!(data[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if pos + 1 >= data.len() {
            break;
        }

        // Check for >>
        if data[pos] == b'>' && data[pos + 1] == b'>' {
            break;
        }

        // Parse key (must be a name /...)
        if data[pos] != b'/' {
            // Skip unknown token
            while pos < data.len()
                && !matches!(data[pos], b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/' | b'[')
            {
                pos += 1;
            }
            continue;
        }

        let name_start = pos + 1;
        pos += 1;
        while pos < data.len()
            && !matches!(
                data[pos],
                b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/' | b'[' | b'(' | b'<'
            )
        {
            pos += 1;
        }
        let key = &data[name_start..pos];

        // Skip whitespace
        while pos < data.len() && matches!(data[pos], b' ' | b'\t' | b'\n' | b'\r') {
            pos += 1;
        }

        if pos >= data.len() {
            break;
        }

        // Parse value (name, ref, array, dict, or skip)
        let value = if data[pos] == b'/' {
            // Name value
            let v_start = pos + 1;
            pos += 1;
            while pos < data.len()
                && !matches!(
                    data[pos],
                    b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/' | b'[' | b'(' | b'<'
                )
            {
                pos += 1;
            }
            PdfObj::Name(&data[v_start..pos])
        } else if data[pos].is_ascii_digit() {
            // Could be integer or ref: N G R
            let n_start = pos;
            while pos < data.len() && data[pos].is_ascii_digit() {
                pos += 1;
            }
            let num_str = std::str::from_utf8(&data[n_start..pos]).ok()?;
            let num: u32 = num_str.parse().ok()?;

            // Skip whitespace
            while pos < data.len() && matches!(data[pos], b' ' | b'\t') {
                pos += 1;
            }

            if pos < data.len() && data[pos].is_ascii_digit() {
                let g_start = pos;
                while pos < data.len() && data[pos].is_ascii_digit() {
                    pos += 1;
                }
                let gen_str = std::str::from_utf8(&data[g_start..pos]).ok()?;
                let gen: u16 = gen_str.parse().ok()?;

                // Skip whitespace
                while pos < data.len() && matches!(data[pos], b' ' | b'\t') {
                    pos += 1;
                }

                if pos < data.len() && data[pos] == b'R' {
                    pos += 1;
                    PdfObj::Ref(num, gen)
                } else {
                    PdfObj::_Other
                }
            } else {
                PdfObj::_Other
            }
        } else {
            // Skip unknown value. Without advancing pos, parse_minimal_dict
            // infinite-loops on `[`, `(`, `<`, `<<` — this was the root cause
            // of every open()-phase hang on the PyMuPDF corpus (36 PDFs).
            pos += skip_value(&data[pos..]);
            PdfObj::_Other
        };

        entries.push((key, value));
    }

    Some(PdfObj::Dict(entries))
}

/// Skip a PDF value at the start of `data`, returning bytes consumed.
/// Handles strings, hex strings, dicts, arrays, and names — anything that
/// could appear as a dict value. Number/keyword/REF tokens fall through to
/// the delimiter-bounded scan.
fn skip_value(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    match data[0] {
        b'(' => skip_paren_string(data),
        b'<' if data.len() > 1 && data[1] == b'<' => skip_dict(data),
        b'<' => skip_hex_string(data),
        b'[' => skip_array(data),
        b'/' => skip_name(data),
        _ => {
            // number / keyword / REF — read until delimiter
            let mut i = 0;
            while i < data.len()
                && !matches!(
                    data[i],
                    b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>' | b'[' | b'(' | b'<'
                )
            {
                i += 1;
            }
            i
        }
    }
}

/// Skip `( ... )` string, respecting nested parens and `\` escapes.
fn skip_paren_string(data: &[u8]) -> usize {
    let mut i = 1;
    let mut depth: i32 = 1;
    while i < data.len() && depth > 0 {
        match data[i] {
            b'\\' => i += 2,
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    i
}

/// Skip `< ... >` hex string. First byte is `<`.
fn skip_hex_string(data: &[u8]) -> usize {
    let mut i = 1;
    while i < data.len() && data[i] != b'>' {
        i += 1;
    }
    if i < data.len() {
        i + 1
    } else {
        i
    }
}

/// Skip `<< ... >>` dict, balanced. Handles nested dicts, hex strings, and
/// paren strings inside.
fn skip_dict(data: &[u8]) -> usize {
    let mut i = 2;
    let mut depth: i32 = 1;
    while i < data.len() && depth > 0 {
        match data[i] {
            b'<' => {
                if i + 1 < data.len() && data[i + 1] == b'<' {
                    depth += 1;
                    i += 2;
                } else {
                    i = (i + skip_hex_string(&data[i..])).max(i + 1);
                }
            }
            b'>' => {
                if i + 1 < data.len() && data[i + 1] == b'>' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            b'(' => i += skip_paren_string(&data[i..]),
            _ => i += 1,
        }
    }
    i
}

/// Skip `[ ... ]` array, balanced. Handles nested arrays, dicts, strings.
fn skip_array(data: &[u8]) -> usize {
    let mut i = 1;
    let mut depth: i32 = 1;
    while i < data.len() && depth > 0 {
        match data[i] {
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                i += 1;
            }
            b'(' => i += skip_paren_string(&data[i..]),
            b'<' => {
                if i + 1 < data.len() && data[i + 1] == b'<' {
                    i += skip_dict(&data[i..]);
                } else {
                    i += skip_hex_string(&data[i..]);
                }
            }
            _ => i += 1,
        }
    }
    i
}

/// Skip `/Name`. First byte is `/`.
fn skip_name(data: &[u8]) -> usize {
    let mut i = 1;
    while i < data.len()
        && !matches!(
            data[i],
            b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>' | b'[' | b'(' | b'<'
        )
    {
        i += 1;
    }
    i
}

fn is_catalog(dict: &[(&[u8], PdfObj<'_>)]) -> bool {
    dict.iter()
        .any(|(k, v)| *k == b"Type" && matches!(v, PdfObj::Name(n) if n == b"Catalog"))
}

fn find_ref_in_dict(dict: &[(&[u8], PdfObj<'_>)], key: &[u8]) -> Option<ObjectId> {
    for (k, v) in dict {
        if *k == key {
            if let PdfObj::Ref(num, gen) = v {
                return Some(ObjectId::new(*num, *gen));
            }
        }
    }
    None
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recovery_basic() {
        let mut pdf = Vec::new();
        pdf.extend_from_slice(b"%PDF-1.4\n");

        // Object 1: Catalog (no proper xref)
        pdf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        // Object 2: Pages
        pdf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        // Object 3: Page
        pdf.extend_from_slice(b"3 0 obj\n<< /Type /Page /Parent 2 0 R >>\nendobj\n");

        // Intentionally no xref table - recovery should still find objects
        let result = recover_xref_by_scan(&pdf);
        assert!(result.is_ok(), "recovery should succeed");

        let xref = result.unwrap();
        assert!(xref.entries.len() >= 3, "should find at least 3 objects");
        assert_eq!(xref.root.num, 1);
    }

    #[test]
    fn test_try_parse_obj_header() {
        let data = b"1 0 obj";
        let result = try_parse_obj_header(data, 4); // position of "obj"
        assert_eq!(result, Some((1, 0, 0)));
    }

    #[test]
    fn test_try_parse_obj_header_gen_nonzero() {
        let data = b"5 3 obj";
        let result = try_parse_obj_header(data, 4);
        assert_eq!(result, Some((5, 3, 0)));
    }
}
