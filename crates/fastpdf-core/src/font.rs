/// Font handling: CMap parsing, width lookup, CID-to-Unicode mapping.
///
/// Implements the full fallback chain:
/// ToUnicode CMap → Encoding differences → raw byte mapping → U+FFFD
use crate::parser::ParseResult;
use crate::types::PdfObject;
use std::collections::HashMap;

/// Parsed font information from a font dictionary.
#[derive(Debug, Clone)]
pub struct FontInfo {
    pub base_font: String,
    pub encoding: Option<String>,
    pub is_type0: bool,
    pub widths: Vec<f64>,
    pub default_width: f64,
    pub cmap: Option<CMap>,
    pub differences: Option<HashMap<u8, Vec<u8>>>,
    /// CIDFont descendant (for Type0 composite fonts)
    pub cid_font: Option<CIDFontInfo>,
}

/// A parsed ToUnicode CMap.
#[derive(Debug, Clone)]
pub struct CMap {
    /// Single character mappings: char code → Unicode string
    pub bfchar: HashMap<Vec<u8>, Vec<u8>>,
    /// Range mappings: (start, end) → Unicode base
    pub bfrange: Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>,
}

/// A contiguous range of CID widths: [c_first, c_first+len) with individual widths.
#[derive(Debug, Clone)]
pub struct CIDWidthRange {
    pub c_first: u32,
    pub widths: Vec<f64>,
}

/// CIDFont descendant info for Type0 composite fonts.
#[derive(Debug, Clone)]
pub struct CIDFontInfo {
    pub base_font: String,
    pub subtype: String, // CIDFontType0 or CIDFontType2
    /// CID width ranges (sorted by c_first for binary search)
    pub width_ranges: Vec<CIDWidthRange>,
    pub default_width: f64,
    pub dw: f64,
    /// CID to GID mapping (for CIDFontType2 TrueType-based)
    pub cid_to_gid: Vec<u8>,
    /// /DW2: default vertical displacement [vy, w1y]
    pub dw2: Option<[f64; 2]>,
    /// /W2: vertical widths
    pub w2: Vec<f64>,
}

impl Default for CIDFontInfo {
    fn default() -> Self {
        Self {
            base_font: String::new(),
            subtype: String::new(),
            width_ranges: Vec::new(),
            default_width: 1000.0,
            dw: 1000.0,
            cid_to_gid: Vec::new(),
            dw2: None,
            w2: Vec::new(),
        }
    }
}

impl CIDFontInfo {
    /// Get CID width from parsed /W ranges using binary search.
    pub fn cid_width(&self, cid: u32) -> f64 {
        // Binary search for the range containing cid
        match self.width_ranges.binary_search_by(|r| {
            if cid < r.c_first {
                std::cmp::Ordering::Greater
            } else if cid >= r.c_first + r.widths.len() as u32 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(idx) => {
                let offset = (cid - self.width_ranges[idx].c_first) as usize;
                self.width_ranges[idx].widths[offset]
            }
            Err(_) => self.dw,
        }
    }
}

impl FontInfo {
    /// Decode a character code to Unicode using the fallback chain.
    pub fn decode_char(&self, code: &[u8]) -> char {
        // 1. Try ToUnicode CMap
        if let Some(cmap) = &self.cmap {
            if let Some(unicode) = cmap.lookup(code) {
                if let Some(c) = bytes_to_char(&unicode) {
                    return c;
                }
            }
        }

        // 2. Try Encoding differences
        if let Some(diffs) = &self.differences {
            if code.len() == 1 {
                if let Some(name) = diffs.get(&code[0]) {
                    if let Some(c) = adobe_glyph_to_char(name) {
                        return c;
                    }
                }
            }
        }

        // 3. Try raw byte (for Latin-1 / ASCII)
        if code.len() == 1 {
            let b = code[0];
            if (0x20..0x7F).contains(&b) {
                return b as char;
            }
            // Latin-1 supplement
            if b >= 0x80 {
                return char::from(b);
            }
        }

        // 4. Fallback
        '\u{FFFD}'
    }

    /// Get the width of a character code in thousandths of a unit.
    pub fn char_width(&self, code: u32) -> f64 {
        let idx = code as usize;
        if idx < self.widths.len() {
            self.widths[idx]
        } else {
            self.default_width
        }
    }
}

impl CMap {
    pub fn lookup(&self, code: &[u8]) -> Option<Vec<u8>> {
        // Direct lookup
        if let Some(unicode) = self.bfchar.get(code) {
            return Some(unicode.clone());
        }

        // Range lookup
        for (start, end, base) in &self.bfrange {
            if code.len() == start.len()
                && code.len() == end.len()
                && code >= start.as_slice()
                && code <= end.as_slice()
            {
                let offset = code_offset(code, start);
                return Some(add_offset(base, offset));
            }
        }

        None
    }
}

fn code_offset(code: &[u8], base: &[u8]) -> u32 {
    let mut offset = 0u32;
    for i in 0..code.len() {
        offset = offset.wrapping_shl(8) | ((code[i] as u32).wrapping_sub(base[i] as u32));
    }
    offset
}

fn add_offset(base: &[u8], offset: u32) -> Vec<u8> {
    let mut result = base.to_vec();
    let mut carry = offset as i64;
    for i in (0..result.len()).rev() {
        let val = result[i] as i64 + carry;
        result[i] = (val & 0xFF) as u8;
        carry = val >> 8;
    }
    result
}

fn bytes_to_char(bytes: &[u8]) -> Option<char> {
    if bytes.len() == 1 {
        return Some(bytes[0] as char);
    }
    if bytes.len() == 2 {
        let code = ((bytes[0] as u32) << 8) | (bytes[1] as u32);
        return char::from_u32(code);
    }
    if bytes.len() == 3 {
        let code = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | (bytes[2] as u32);
        return char::from_u32(code);
    }
    // 4 bytes: could be UTF-16BE surrogate pair or direct 4-byte encoding
    if bytes.len() == 4 {
        let code = ((bytes[0] as u32) << 24)
            | ((bytes[1] as u32) << 16)
            | ((bytes[2] as u32) << 8)
            | (bytes[3] as u32);
        // Check if it's a UTF-16BE surrogate pair (0xD800..0xDBFF followed by 0xDC00..0xDFFF)
        if (code >> 16) >= 0xD800
            && (code >> 16) <= 0xDBFF
            && (code & 0xFFFF) >= 0xDC00
            && (code & 0xFFFF) <= 0xDFFF
        {
            let high = (code >> 16) - 0xD800;
            let low = (code & 0xFFFF) - 0xDC00;
            let unicode = 0x10000 + (high << 10) + low;
            return char::from_u32(unicode);
        }
        // Direct 4-byte encoding
        return char::from_u32(code);
    }
    None
}

// ─── CMap parsing ───

/// Parse a ToUnicode CMap from stream data.
pub fn parse_cmap(data: &[u8]) -> ParseResult<CMap> {
    let mut bfchar = HashMap::new();
    let mut bfrange = Vec::new();
    let mut pos = 0;
    let s = std::str::from_utf8(data).unwrap_or("");

    // Find beginbfchar ... endbfchar blocks
    while let Some(start) = find_token(s, pos, "beginbfchar") {
        let after = start + "beginbfchar".len();
        let end = find_token(s, after, "endbfchar").unwrap_or(s.len());
        let block = &s[after..end];
        parse_bfchar_block(block, &mut bfchar);
        pos = end + "endbfchar".len();
    }

    // Find beginbfrange ... endbfrange blocks
    pos = 0;
    while let Some(start) = find_token(s, pos, "beginbfrange") {
        let after = start + "beginbfrange".len();
        let end = find_token(s, after, "endbfrange").unwrap_or(s.len());
        let block = &s[after..end];
        parse_bfrange_block(block, &mut bfrange);
        pos = end + "endbfrange".len();
    }

    Ok(CMap { bfchar, bfrange })
}

fn find_token(s: &str, from: usize, token: &str) -> Option<usize> {
    s[from..].find(token).map(|i| i + from)
}

fn parse_bfchar_block(block: &str, map: &mut HashMap<Vec<u8>, Vec<u8>>) {
    let tokens: Vec<&str> = block.split_whitespace().collect();
    let mut i = 0;
    while i + 1 < tokens.len() {
        let src = parse_hex_tokens(tokens[i]);
        let dst = parse_hex_tokens(tokens[i + 1]);
        if !src.is_empty() && !dst.is_empty() {
            map.insert(src, dst);
        }
        i += 2;
    }
}

fn parse_bfrange_block(block: &str, ranges: &mut Vec<(Vec<u8>, Vec<u8>, Vec<u8>)>) {
    let tokens: Vec<&str> = block.split_whitespace().collect();
    let mut i = 0;
    while i + 2 < tokens.len() {
        let start = parse_hex_tokens(tokens[i]);
        let end = parse_hex_tokens(tokens[i + 1]);
        let dst = parse_hex_tokens(tokens[i + 2]);
        if !start.is_empty() && !end.is_empty() && !dst.is_empty() {
            ranges.push((start, end, dst));
        }
        i += 3;
    }
}

fn parse_hex_tokens(s: &str) -> Vec<u8> {
    let s = s.trim_start_matches('<').trim_end_matches('>');
    let mut result = Vec::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i + 1 < chars.len() {
        let hi = hex_char_val(chars[i]);
        let lo = hex_char_val(chars[i + 1]);
        result.push((hi << 4) | lo);
        i += 2;
    }
    result
}

fn hex_char_val(c: char) -> u8 {
    match c {
        '0'..='9' => (c as u8) - b'0',
        'a'..='f' => (c as u8) - b'a' + 10,
        'A'..='F' => (c as u8) - b'A' + 10,
        _ => 0,
    }
}

// ─── Font info extraction ───

/// Extract FontInfo from a font dictionary object.
pub fn extract_font_info(font_obj: &PdfObject<'_>) -> FontInfo {
    let base_font = font_obj
        .get(b"BaseFont")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    let encoding = font_obj
        .get(b"Encoding")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string());

    let subtype = font_obj
        .get(b"Subtype")
        .and_then(|v| v.as_name())
        .unwrap_or(b"");

    let is_type0 = subtype == b"Type0";

    // Extract /Widths array
    let widths = font_obj
        .get(b"Widths")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|item| item.as_f64()).collect())
        .unwrap_or_default();

    let default_width = font_obj
        .get(b"DW")
        .and_then(|v| v.as_f64())
        .unwrap_or(1000.0);

    // Extract /Differences from Encoding dict
    let differences = extract_differences(font_obj);

    FontInfo {
        base_font,
        encoding,
        is_type0,
        widths,
        default_width,
        cmap: None, // Populated later from ToUnicode stream
        differences,
        cid_font: None, // Populated for Type0 fonts
    }
}

fn extract_differences(font_obj: &PdfObject<'_>) -> Option<HashMap<u8, Vec<u8>>> {
    let encoding = font_obj.get(b"Encoding")?;
    let dict = match encoding {
        PdfObject::Dict(d) => d,
        PdfObject::Ref(_) => return None, // Would need to resolve via get_object
        _ => return None,
    };

    let diff_array = dict
        .iter()
        .find(|(k, _)| *k == b"Differences")?
        .1
        .as_array()?;

    let mut result = HashMap::new();
    let mut current_code: u8 = 0;

    for item in diff_array {
        match item {
            PdfObject::Integer(n) => {
                current_code = *n as u8;
            }
            PdfObject::Name(name) => {
                result.insert(current_code, name.to_vec());
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Extract differences from encoding dict, resolving references.
fn extract_differences_with_resolve<'a>(
    font_obj: &PdfObject<'a>,
    get_object: &impl Fn(u32) -> ParseResult<PdfObject<'a>>,
) -> Option<HashMap<u8, Vec<u8>>> {
    let encoding = font_obj.get(b"Encoding")?;

    // Resolve reference if needed
    let resolved;
    let dict = match encoding {
        PdfObject::Dict(d) => d,
        PdfObject::Ref(r) => {
            resolved = get_object(r.num).ok()?;
            match &resolved {
                PdfObject::Dict(d) => d,
                _ => return None,
            }
        }
        _ => return None,
    };

    let diff_array = dict
        .iter()
        .find(|(k, _)| *k == b"Differences")?
        .1
        .as_array()?;

    let mut result = HashMap::new();
    let mut current_code: u8 = 0;

    for item in diff_array {
        match item {
            PdfObject::Integer(n) => {
                current_code = *n as u8;
            }
            PdfObject::Name(name) => {
                result.insert(current_code, name.to_vec());
                current_code = current_code.wrapping_add(1);
            }
            _ => {}
        }
    }

    if result.is_empty() {
        None
    } else {
        Some(result)
    }
}

/// Build a complete font map from a page's /Resources /Font dictionary.
pub fn build_font_map<'a>(
    resources: &PdfObject<'a>,
    get_object: impl Fn(u32) -> ParseResult<PdfObject<'a>>,
) -> HashMap<String, FontInfo> {
    let mut result = HashMap::new();

    let fonts_dict = match resources.get(b"Font") {
        Some(PdfObject::Dict(d)) => d,
        _ => return result,
    };

    for (name, font_ref) in fonts_dict {
        let font_name = String::from_utf8_lossy(name).to_string();

        let font_obj = match font_ref {
            PdfObject::Ref(r) => match get_object(r.num) {
                Ok(obj) => obj,
                _ => continue,
            },
            PdfObject::Dict(_) => font_ref.clone(),
            _ => continue,
        };

        let mut info = extract_font_info(&font_obj);

        // Try to resolve encoding differences if not already extracted
        if info.differences.is_none() {
            info.differences = extract_differences_with_resolve(&font_obj, &get_object);
        }

        // Try to get ToUnicode CMap
        if let Some(PdfObject::Ref(r)) = font_obj.get(b"ToUnicode") {
            if let Ok(tounicode_obj) = get_object(r.num) {
                if let PdfObject::Stream { data, dict } = &tounicode_obj {
                    // Decompress if needed
                    let filter = dict.iter().find(|(k, _)| *k == b"Filter").map(|(_, v)| v);
                    let cmap_data = match filter {
                        Some(f) => crate::parser::xref::decompress_stream(data, f)
                            .unwrap_or_else(|_| data.to_vec()),
                        None => data.to_vec(),
                    };

                    if let Ok(cmap) = parse_cmap(&cmap_data) {
                        info.cmap = Some(cmap);
                    }
                }
            }
        }

        // For Type0 fonts, resolve CIDFont descendant
        if info.is_type0 {
            if let Some(PdfObject::Array(descendants)) = font_obj.get(b"DescendantFonts") {
                if let Some(PdfObject::Ref(r)) = descendants.first() {
                    if let Ok(cid_font_obj) = get_object(r.num) {
                        info.cid_font = Some(extract_cid_font_info(&cid_font_obj));
                    }
                }
            }
        }

        result.insert(font_name, info);
    }

    result
}

/// Extract CIDFont info from a CIDFont dictionary.
fn extract_cid_font_info(cid_obj: &PdfObject<'_>) -> CIDFontInfo {
    let base_font = cid_obj
        .get(b"BaseFont")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    let subtype = cid_obj
        .get(b"Subtype")
        .and_then(|v| v.as_name())
        .map(|n| String::from_utf8_lossy(n).to_string())
        .unwrap_or_default();

    let dw = cid_obj
        .get(b"DW")
        .and_then(|v| v.as_f64())
        .unwrap_or(1000.0);

    // Parse /W array into ranges: format is c_first c_last w1 w2 ... wn OR c [w1 w2 ... wn]
    let width_ranges = parse_cid_widths(cid_obj);

    // Parse /CIDToGIDMap (for CIDFontType2)
    let cid_to_gid = match cid_obj.get(b"CIDToGIDMap") {
        Some(PdfObject::Stream { data, .. }) => data.to_vec(),
        _ => Vec::new(),
    };

    let dw2 = cid_obj.get(b"DW2").and_then(|v| {
        v.as_array().and_then(|arr| {
            if arr.len() >= 2 {
                Some([
                    arr[0].as_f64().unwrap_or(0.0),
                    arr[1].as_f64().unwrap_or(0.0),
                ])
            } else {
                None
            }
        })
    });

    let w2 = cid_obj
        .get(b"W2")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|item| item.as_f64()).collect())
        .unwrap_or_default();

    CIDFontInfo {
        base_font,
        subtype,
        width_ranges,
        default_width: dw,
        dw,
        cid_to_gid,
        dw2,
        w2,
    }
}

/// Parse CIDFont /W array into width ranges.
/// Format: c_first c_last w1 w2 ... wn (range) OR c [w1 w2 ... wn] (array)
fn parse_cid_widths(cid_obj: &PdfObject<'_>) -> Vec<CIDWidthRange> {
    let w_array = match cid_obj.get(b"W") {
        Some(PdfObject::Array(a)) => a,
        _ => return Vec::new(),
    };

    let mut ranges = Vec::new();
    let mut i = 0;
    while i < w_array.len() {
        let c_first = match &w_array[i] {
            PdfObject::Integer(n) => *n as u32,
            _ => {
                i += 1;
                continue;
            }
        };

        if i + 1 >= w_array.len() {
            break;
        }

        // Check if next is an array (c [w1 w2 ... wn]) or integer (c_first c_last w...)
        match &w_array[i + 1] {
            PdfObject::Array(arr) => {
                // Individual widths: c [w1 w2 ... wn]
                let widths: Vec<f64> = arr.iter().map(|w| w.as_f64().unwrap_or(0.0)).collect();
                if !widths.is_empty() {
                    ranges.push(CIDWidthRange { c_first, widths });
                }
                i += 2;
            }
            PdfObject::Integer(c_last) => {
                // Range: c_first c_last w1 w2 ... wn
                let c_last = *c_last as u32;
                let count = (c_last - c_first + 1) as usize;
                let mut widths = Vec::with_capacity(count);
                for j in 0..count {
                    if i + 2 + j < w_array.len() {
                        widths.push(w_array[i + 2 + j].as_f64().unwrap_or(0.0));
                    } else {
                        widths.push(0.0);
                    }
                }
                ranges.push(CIDWidthRange { c_first, widths });
                i += 2 + count;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Sort by c_first for binary search
    ranges.sort_by_key(|r| r.c_first);
    ranges
}

// ─── Adobe Glyph List (subset) ───

fn adobe_glyph_to_char(name: &[u8]) -> Option<char> {
    let s = std::str::from_utf8(name).ok()?;
    match s {
        "space" => Some(' '),
        "exclam" => Some('!'),
        "quotedbl" => Some('"'),
        "numbersign" => Some('#'),
        "dollar" => Some('$'),
        "percent" => Some('%'),
        "ampersand" => Some('&'),
        "quotesingle" => Some('\''),
        "parenleft" => Some('('),
        "parenright" => Some(')'),
        "asterisk" => Some('*'),
        "plus" => Some('+'),
        "comma" => Some(','),
        "hyphen" => Some('-'),
        "period" => Some('.'),
        "slash" => Some('/'),
        "zero" => Some('0'),
        "one" => Some('1'),
        "two" => Some('2'),
        "three" => Some('3'),
        "four" => Some('4'),
        "five" => Some('5'),
        "six" => Some('6'),
        "seven" => Some('7'),
        "eight" => Some('8'),
        "nine" => Some('9'),
        "colon" => Some(':'),
        "semicolon" => Some(';'),
        "less" => Some('<'),
        "equal" => Some('='),
        "greater" => Some('>'),
        "question" => Some('?'),
        "at" => Some('@'),
        "bracketleft" => Some('['),
        "backslash" => Some('\\'),
        "bracketright" => Some(']'),
        "asciicircum" => Some('^'),
        "underscore" => Some('_'),
        "grave" => Some('`'),
        "braceleft" => Some('{'),
        "bar" => Some('|'),
        "braceright" => Some('}'),
        "asciitilde" => Some('~'),
        "bullet" => Some('•'),
        "dagger" => Some('†'),
        "daggerdbl" => Some('‡'),
        "ellipsis" => Some('…'),
        "emdash" => Some('—'),
        "endash" => Some('–'),
        "fi" => Some('ﬁ'),
        "fl" => Some('ﬂ'),
        "fraction" => Some('⁄'),
        "guilsinglleft" => Some('‹'),
        "guilsinglright" => Some('›'),
        "minus" => Some('−'),
        "mu" => Some('μ'),
        "multiply" => Some('×'),
        "onehalf" => Some('½'),
        "onequarter" => Some('¼'),
        "onesuperior" => Some('¹'),
        "plusminus" => Some('±'),
        "quotedblbase" => Some('„'),
        "quotedblleft" => Some('\u{201C}'),
        "quotedblright" => Some('\u{201D}'),
        "quoteleft" => Some('\u{2018}'),
        "quoteright" => Some('\u{2019}'),
        "quotesinglbase" => Some('‚'),
        "registered" => Some('®'),
        "threequarters" => Some('¾'),
        "threesuperior" => Some('³'),
        "trademark" => Some('™'),
        "twosuperior" => Some('²'),
        "Euro" => Some('€'),
        "Lslash" => Some('Ł'),
        "OE" => Some('Œ'),
        "Scaron" => Some('Š'),
        "Zcaron" => Some('Ž'),
        "lslash" => Some('ł'),
        "oe" => Some('œ'),
        "scaron" => Some('š'),
        "zcaron" => Some('ž'),
        // Greek letters (common in mathematics)
        "Alpha" => Some('Α'),
        "Beta" => Some('Β'),
        "Gamma" => Some('Γ'),
        "Delta" => Some('Δ'),
        "Epsilon" => Some('Ε'),
        "Zeta" => Some('Ζ'),
        "Eta" => Some('Η'),
        "Theta" => Some('Θ'),
        "Iota" => Some('Ι'),
        "Kappa" => Some('Κ'),
        "Lambda" => Some('Λ'),
        "Mu" => Some('Μ'),
        "Nu" => Some('Ν'),
        "Xi" => Some('Ξ'),
        "Omicron" => Some('Ο'),
        "Pi" => Some('Π'),
        "Rho" => Some('Ρ'),
        "Sigma" => Some('Σ'),
        "Tau" => Some('Τ'),
        "Upsilon" => Some('Υ'),
        "Phi" => Some('Φ'),
        "Chi" => Some('Χ'),
        "Psi" => Some('Ψ'),
        "Omega" => Some('Ω'),
        "alpha" => Some('α'),
        "beta" => Some('β'),
        "gamma" => Some('γ'),
        "delta" => Some('δ'),
        "epsilon" => Some('ε'),
        "zeta" => Some('ζ'),
        "eta" => Some('η'),
        "theta" => Some('θ'),
        "iota" => Some('ι'),
        "kappa" => Some('κ'),
        "lambda" => Some('λ'),
        "nu" => Some('ν'),
        "xi" => Some('ξ'),
        "omicron" => Some('ο'),
        "pi" => Some('π'),
        "rho" => Some('ρ'),
        "sigma" => Some('σ'),
        "tau" => Some('τ'),
        "upsilon" => Some('υ'),
        "phi" => Some('φ'),
        "chi" => Some('χ'),
        "psi" => Some('ψ'),
        "omega" => Some('ω'),
        "phi1" => Some('ϕ'),
        "epsilon1" => Some('ϵ'),
        // Mathematical symbols
        "infinity" => Some('∞'),
        "partialdiff" => Some('∂'),
        "nabla" => Some('∇'),
        "integral" => Some('∫'),
        "product" => Some('∏'),
        "summation" => Some('∑'),
        "radical" => Some('√'),
        "sim" => Some('∼'),
        "congruent" => Some('≅'),
        "approxequal" => Some('≈'),
        "notequal" => Some('≠'),
        "lessequal" => Some('≤'),
        "greaterequal" => Some('≥'),
        "logicalnot" => Some('¬'),
        "logicaland" => Some('∧'),
        "logicalor" => Some('∨'),
        "element" => Some('∈'),
        "notelement" => Some('∉'),
        "subset" => Some('⊂'),
        "superset" => Some('⊃'),
        "subseteq" => Some('⊆'),
        "supersetequal" => Some('⊇'),
        "union" => Some('∪'),
        "intersection" => Some('∩'),
        "emptyset" => Some('∅'),
        "forall" => Some('∀'),
        "exist" => Some('∃'),
        "angle" => Some('∠'),
        "perpendicular" => Some('⊥'),
        "parallel" => Some('∥'),
        "arrowleft" => Some('←'),
        "arrowright" => Some('→'),
        "arrowup" => Some('↑'),
        "arrowdown" => Some('↓'),
        "arrowboth" => Some('↔'),
        "arrowdblleft" => Some('⇐'),
        "arrowdblright" => Some('⇒'),
        "arrowdblup" => Some('⇑'),
        "arrowdbldown" => Some('⇓'),
        // Common diacritical characters
        "Aacute" => Some('Á'),
        "Agrave" => Some('À'),
        "Acircumflex" => Some('Â'),
        "Atilde" => Some('Ã'),
        "Adieresis" => Some('Ä'),
        "Aring" => Some('Å'),
        "Ccedilla" => Some('Ç'),
        "Eacute" => Some('É'),
        "Egrave" => Some('È'),
        "Ecircumflex" => Some('Ê'),
        "Edieresis" => Some('Ë'),
        "Iacute" => Some('Í'),
        "Igrave" => Some('Ì'),
        "Icircumflex" => Some('Î'),
        "Idieresis" => Some('Ï'),
        "Ntilde" => Some('Ñ'),
        "Oacute" => Some('Ó'),
        "Ograve" => Some('Ò'),
        "Ocircumflex" => Some('Ô'),
        "Otilde" => Some('Õ'),
        "Odieresis" => Some('Ö'),
        "Uacute" => Some('Ú'),
        "Ugrave" => Some('Ù'),
        "Ucircumflex" => Some('Û'),
        "Udieresis" => Some('Ü'),
        "Yacute" => Some('Ý'),
        "aacute" => Some('á'),
        "agrave" => Some('à'),
        "acircumflex" => Some('â'),
        "atilde" => Some('ã'),
        "adieresis" => Some('ä'),
        "aring" => Some('å'),
        "ccedilla" => Some('ç'),
        "eacute" => Some('é'),
        "egrave" => Some('è'),
        "ecircumflex" => Some('ê'),
        "edieresis" => Some('ë'),
        "iacute" => Some('í'),
        "igrave" => Some('ì'),
        "icircumflex" => Some('î'),
        "idieresis" => Some('ï'),
        "ntilde" => Some('ñ'),
        "oacute" => Some('ó'),
        "ograve" => Some('ò'),
        "ocircumflex" => Some('ô'),
        "otilde" => Some('õ'),
        "odieresis" => Some('ö'),
        "uacute" => Some('ú'),
        "ugrave" => Some('ù'),
        "ucircumflex" => Some('û'),
        "udieresis" => Some('ü'),
        "yacute" => Some('ý'),
        "ydieresis" => Some('ÿ'),
        // Default: try to parse as uniXXXX
        _ => {
            if s.starts_with("uni") && s.len() >= 7 {
                let hex = &s[3..];
                if let Ok(code) = u32::from_str_radix(hex, 16) {
                    return char::from_u32(code);
                }
            }
            None
        }
    }
}

// ─── Tests ───

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cmap_parse_simple() {
        let cmap_data = b"1 beginbfchar\n<41> <42>\nendbfchar\n";
        let cmap = parse_cmap(cmap_data).unwrap();
        assert_eq!(cmap.bfchar.len(), 1);
        assert_eq!(cmap.lookup(&[0x41]), Some(vec![0x42]));
    }

    #[test]
    fn test_cmap_range() {
        let cmap_data = b"1 beginbfrange\n<41> <43> <61>\nendbfrange\n";
        let cmap = parse_cmap(cmap_data).unwrap();
        assert_eq!(cmap.lookup(&[0x41]), Some(vec![0x61])); // A → a
        assert_eq!(cmap.lookup(&[0x42]), Some(vec![0x62])); // B → b
        assert_eq!(cmap.lookup(&[0x43]), Some(vec![0x63])); // C → c
        assert_eq!(cmap.lookup(&[0x44]), None);
    }

    #[test]
    fn test_adobe_glyph_lookup() {
        assert_eq!(adobe_glyph_to_char(b"space"), Some(' '));
        assert_eq!(adobe_glyph_to_char(b"A"), None);
        assert_eq!(adobe_glyph_to_char(b"Euro"), Some('€'));
        assert_eq!(adobe_glyph_to_char(b"uni0041"), Some('A'));
    }

    #[test]
    fn test_font_decode_latin() {
        let info = FontInfo {
            base_font: "Helvetica".into(),
            encoding: None,
            is_type0: false,
            widths: vec![],
            default_width: 600.0,
            cmap: None,
            differences: None,
            cid_font: None,
        };
        assert_eq!(info.decode_char(&[0x41]), 'A');
        assert_eq!(info.decode_char(&[0x20]), ' ');
    }

    #[test]
    fn test_font_decode_with_cmap() {
        let mut bfchar = HashMap::new();
        bfchar.insert(vec![0x00, 0x41], vec![0x00, 0x61]); // A → a

        let info = FontInfo {
            base_font: "TestFont".into(),
            encoding: None,
            is_type0: false,
            widths: vec![],
            default_width: 1000.0,
            cmap: Some(CMap {
                bfchar,
                bfrange: vec![],
            }),
            differences: None,
            cid_font: None,
        };
        assert_eq!(info.decode_char(&[0x00, 0x41]), 'a');
    }
}
